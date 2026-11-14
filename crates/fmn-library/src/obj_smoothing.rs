//! Missing OBJ corner normals, accumulated over source faces before their
//! triangulation can bias the weights. No renderer-specific representation.

use std::collections::{BTreeMap, btree_map::Entry};
use std::ops::Range;

use super::{ObjCorner, ObjError, ObjLimits, ObjMesh, Vec3, cross, sub};

/// An area vector in units of `scale²`. Comparing scales before multiplying
/// keeps finite extreme coordinates and very small meshes out of overflow.
#[derive(Clone, Copy, Default)]
struct Area {
    scale: f64,
    vector: Vec3,
}

impl Area {
    fn face(mesh: &ObjMesh, corners: &[ObjCorner]) -> Self {
        let origin = mesh.vertices[corners[0].vertex];
        let translated = corners.iter().all(|corner| {
            sub(mesh.vertices[corner.vertex], origin)
                .iter()
                .all(|value| value.is_finite())
        });
        let raw = |corner: &ObjCorner| {
            let point = mesh.vertices[corner.vertex];
            if translated {
                sub(point, origin)
            } else {
                point
            }
        };
        let scale = corners
            .iter()
            .flat_map(raw)
            .map(f64::abs)
            .fold(0.0, f64::max);
        if scale == 0.0 {
            return Self::default();
        }
        let point = |corner: &ObjCorner| {
            let value = raw(corner).map(|value| value / scale);
            if translated {
                value
            } else {
                sub(value, origin.map(|value| value / scale))
            }
        };
        let mut vector = [0.0; 3];
        for index in 0..corners.len() {
            let area = cross(
                point(&corners[index]),
                point(&corners[(index + 1) % corners.len()]),
            );
            for axis in 0..3 {
                vector[axis] += area[axis];
            }
        }
        Self { scale, vector }
    }

    fn add(&mut self, area: Self) {
        // A large collinear face has a scale but no area. Letting its scale
        // rescale an existing contribution can erase a valid small face.
        if area.vector == [0.0; 3] {
            return;
        }
        // Exact cancellation leaves no area to rescale either. Restart from
        // the next real contribution, even if its geometry is much smaller.
        if self.vector == [0.0; 3] {
            *self = area;
            return;
        }
        if self.scale < area.scale {
            let factor = self.scale / area.scale;
            self.vector = self.vector.map(|value| value * factor * factor);
            self.scale = area.scale;
        }
        let factor = area.scale / self.scale;
        for axis in 0..3 {
            self.vector[axis] += area.vector[axis] * factor * factor;
        }
    }

    fn normal(self) -> Vec3 {
        let largest = self.vector.into_iter().map(f64::abs).fold(0.0, f64::max);
        if largest == 0.0 {
            return [0.0; 3];
        }
        let bounded = self.vector.map(|value| value / largest);
        let length = bounded
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        bounded.map(|value| value / length)
    }
}

struct SmoothFace {
    group: u64,
    line: usize,
    triangles: Range<usize>,
    area: Area,
}

struct SmoothNormal {
    area: Area,
    last_face: usize,
    output: usize,
}

#[derive(Default)]
pub(super) struct Smoothing {
    group: u64,
    faces: Vec<SmoothFace>,
    // Only vertex/group pairs with at least one missing normal allocate a
    // generated normal. Fully explicit meshes need no synthetic normal budget.
    normals: BTreeMap<(u64, usize), SmoothNormal>,
}

impl Smoothing {
    pub(super) fn normal_count(&self) -> usize {
        self.normals.len()
    }

    pub(super) fn set_group(&mut self, args: &[&str], line: usize) -> Result<(), ObjError> {
        self.group = match args {
            ["off"] => 0,
            ["on"] => 1,
            [value] if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                value
                    .parse::<u64>()
                    .map_err(|_| ObjError::BadSmoothingGroup {
                        line,
                        token: (*value).to_owned(),
                    })?
            }
            _ => {
                return Err(ObjError::BadSmoothingGroup {
                    line,
                    token: args.join(" "),
                });
            }
        };
        Ok(())
    }

    pub(super) fn record_face(
        &mut self,
        mesh: &ObjMesh,
        corners: &[ObjCorner],
        triangles: Range<usize>,
        line: usize,
        limits: &ObjLimits,
    ) -> Result<(), ObjError> {
        if self.group == 0 {
            return Ok(());
        }
        for corner in corners.iter().filter(|corner| corner.normal.is_none()) {
            let key = (self.group, corner.vertex);
            let limit_reached =
                mesh.normals.len().saturating_add(self.normals.len()) >= limits.max_normals;
            if let Entry::Vacant(entry) = self.normals.entry(key) {
                if limit_reached {
                    return Err(ObjError::TooManyNormals {
                        line,
                        limit: limits.max_normals,
                    });
                }
                entry.insert(SmoothNormal {
                    area: Area::default(),
                    last_face: usize::MAX,
                    output: 0,
                });
            }
        }
        // Every face already consumed at least one admitted triangle. Retain
        // its bounded area/range, including explicit-normal faces which may
        // contribute to a missing normal on a later face in the same group.
        self.faces
            .try_reserve(1)
            .map_err(|_| ObjError::AllocationFailed {
                line,
                resource: "smoothing face",
                requested: self.faces.len().saturating_add(1),
            })?;
        self.faces.push(SmoothFace {
            group: self.group,
            line,
            triangles,
            area: Area::face(mesh, corners),
        });
        Ok(())
    }

    pub(super) fn finish(mut self, mesh: &mut ObjMesh) -> Result<(), ObjError> {
        if self.normals.is_empty() {
            return Ok(());
        }
        for (index, face) in self.faces.iter().enumerate() {
            for corner in mesh.triangles[face.triangles.clone()].iter().flatten() {
                if let Some(normal) = self.normals.get_mut(&(face.group, corner.vertex)) {
                    // A triangulation diagonal can repeat a vertex many times;
                    // the source polygon contributes its full area just once.
                    if normal.last_face != index {
                        normal.area.add(face.area);
                        normal.last_face = index;
                    }
                }
            }
        }
        mesh.normals
            .try_reserve_exact(self.normals.len())
            .map_err(|_| ObjError::AllocationFailed {
                line: self.faces.first().map_or(1, |face| face.line),
                resource: "generated normal",
                requested: self.normals.len(),
            })?;
        for normal in self.normals.values_mut() {
            normal.output = mesh.normals.len();
            mesh.normals.push(normal.area.normal());
        }
        for face in self.faces {
            for corner in mesh.triangles[face.triangles].iter_mut().flatten() {
                if corner.normal.is_none() {
                    let normal = self.normals.get(&(face.group, corner.vertex)).ok_or(
                        ObjError::InvalidPolygon {
                            line: face.line,
                            reason: "missing admitted smoothing normal",
                        },
                    )?;
                    corner.normal = Some(normal.output);
                }
            }
        }
        Ok(())
    }
}
