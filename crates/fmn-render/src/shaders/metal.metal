// Lumen's standard-only Metal annex.
//
// The host derives these flat arrays from the same prepared FrameJob consumed
// by the CPU engines. One threadgroup owns one fine tile; one thread owns one
// output pixel and walks the tile's CSR command run in painter order. There are
// no atomics, unordered appends, or intermediate compositing surfaces.

#include <metal_stdlib>
using namespace metal;

#define SEGMENT_STRIDE 26
#define SEGMENT_ARC_INTERVALS 16
#define SEGMENT_LINEAR 25u
#define PIECE_STRIDE 6
#define JOIN_STRIDE 13
#define STATION_STRIDE 3
#define DRAW_U32_STRIDE 10
#define DRAW_F32_STRIDE 8
#define STYLE_STRIDE 20

#define DRAW_FILL 1u
#define DRAW_STROKE 2u
#define STROKE_BEHIND 4u
#define FLAT_FILL 8u
#define CLASS_INTERIOR 1u
#define STATUS_COMPLETE 0x464d4e4du

#define DEGENERATE_REL 1e-6f
#define MIN_AA_WIDTH 1e-8f
#define MITER_LIMIT 3.1622776601683795f

static float sign_or_positive(float x) {
    return (x >= 0.0f) ? 1.0f : -1.0f;
}

static float fmin_(float a, float b) {
    return (a < b) ? a : b;
}

static float fmax_(float a, float b) {
    return (a > b) ? a : b;
}

// MSL has no cbrt. This is the same named, measured annex divergence as G0-8.
static float cbrt_f(float x) {
    return sign(x) * pow(fabs(x), 1.0f / 3.0f);
}

static int solve_quadratic(float a2, float a1, float a0, thread float *out) {
    float scale = fmax_(fmax_(fabs(a2), fabs(a1)), fabs(a0));
    if (scale <= 0.0f) return 0;
    float tolerance = DEGENERATE_REL * scale;
    if (fabs(a2) <= tolerance) {
        if (fabs(a1) <= tolerance) return 0;
        out[0] = -a0 / a1;
        return 1;
    }
    float discriminant = a1 * a1 - 4.0f * a2 * a0;
    if (discriminant < 0.0f) return 0;
    float root = sqrt(discriminant);
    float q = -0.5f * (a1 + sign_or_positive(a1) * root);
    if (q == 0.0f) {
        out[0] = 0.0f;
        out[1] = 0.0f;
        return 2;
    }
    out[0] = q / a2;
    out[1] = a0 / q;
    return 2;
}

static int solve_cubic(
    float a3,
    float a2,
    float a1,
    float a0,
    thread float *out
) {
    float scale = fmax_(fmax_(fabs(a3), fabs(a2)), fmax_(fabs(a1), fabs(a0)));
    if (scale <= 0.0f) return 0;
    float tolerance = DEGENERATE_REL * scale;
    if (fabs(a3) <= tolerance) return solve_quadratic(a2, a1, a0, out);

    float b = a2 / a3;
    float c = a1 / a3;
    float d = a0 / a3;
    float shift = b / 3.0f;
    float p = c - b * b / 3.0f;
    float q = 2.0f * b * b * b / 27.0f - b * c / 3.0f + d;

    if (fabs(p) <= DEGENERATE_REL * fmax_(fmax_(fabs(p), fabs(q)), 1.0f)) {
        out[0] = cbrt_f(-q) - shift;
        return 1;
    }
    float e1 = q * q / 4.0f;
    float e2 = p * p * p / 27.0f;
    float discriminant = e1 + e2;
    if (fabs(discriminant) <= sqrt(DEGENERATE_REL) * (fabs(e1) + fabs(e2))) {
        out[0] = 3.0f * q / p - shift;
        out[1] = -1.5f * q / p - shift;
        return 2;
    }
    if (discriminant > 0.0f) {
        float root = sqrt(discriminant);
        out[0] =
            cbrt_f(-q / 2.0f + root) + cbrt_f(-q / 2.0f - root) - shift;
        return 1;
    }
    float magnitude = 2.0f * sqrt(-p / 3.0f);
    float argument = clamp((3.0f * q) / (p * magnitude), -1.0f, 1.0f);
    float phase = acos(argument) / 3.0f;
    const float THIRD_TURN = 2.0943951023931953f;
    out[0] = magnitude * cos(phase) - shift;
    out[1] = magnitude * cos(phase - THIRD_TURN) - shift;
    out[2] = magnitude * cos(phase - 2.0f * THIRD_TURN) - shift;
    return 3;
}

// Distance and nearest t for a screen-space quadratic.
static float2 distance_to_quadratic(
    float2 point,
    float2 p0,
    float2 p1,
    float2 p2
) {
    float2 a = p0 - point;
    float2 b = 2.0f * (p1 - p0);
    float2 c = (p2 - p1) - (p1 - p0);
    float cc = dot(c, c);
    float bc = dot(b, c);
    float bb = dot(b, b);
    float ac = dot(a, c);
    float ab = dot(a, b);

    float roots[3];
    int count = solve_cubic(2.0f * cc, 3.0f * bc, bb + 2.0f * ac, ab, roots);
    float best_t = 0.0f;
    float best_distance_squared = dot(a, a);
    float2 end = a + b + c;
    float end_distance_squared = dot(end, end);
    if (end_distance_squared < best_distance_squared) {
        best_distance_squared = end_distance_squared;
        best_t = 1.0f;
    }
    for (int index = 0; index < count; index++) {
        float t = roots[index];
        if (t < 0.0f || t > 1.0f) continue;
        float2 value = a + b * t + c * t * t;
        float distance_squared = dot(value, value);
        if (distance_squared < best_distance_squared) {
            best_distance_squared = distance_squared;
            best_t = t;
        }
    }
    return float2(sqrt(best_distance_squared), best_t);
}

// Exact distance to the image of a host-proven monotone linear quadratic.
static float2 distance_to_capsule(float2 point, float2 p0, float2 p2) {
    float2 chord = p2 - p0;
    float denominator = dot(chord, chord);
    float t = (denominator <= 0.0f)
        ? 0.0f
        : clamp(dot(point - p0, chord) / denominator, 0.0f, 1.0f);
    return float2(length(point - (p0 + chord * t)), t);
}

static float segment_arc_fraction(
    device const float *segments,
    uint segment,
    float t
) {
    uint base = segment * SEGMENT_STRIDE + 8u;
    float position = clamp(t, 0.0f, 1.0f) * (float)SEGMENT_ARC_INTERVALS;
    uint interval = min((uint)floor(position), (uint)(SEGMENT_ARC_INTERVALS - 1));
    float local = position - (float)interval;
    return mix(segments[base + interval], segments[base + interval + 1u], local);
}

static float segment_path_parameter(
    device const float *segments,
    uint segment,
    float t
) {
    uint base = segment * SEGMENT_STRIDE;
    float fraction = (segments[base + SEGMENT_LINEAR] != 0.0f)
        ? clamp(t, 0.0f, 1.0f)
        : segment_arc_fraction(segments, segment, t);
    return mix(segments[base + 6u], segments[base + 7u], fraction);
}

struct Coefficients {
    float ax;
    float bx;
    float cx;
    float ay;
    float by;
    float cy;
};

static Coefficients piece_coefficients(
    device const float *pieces,
    uint index,
    float scale
) {
    uint base = index * PIECE_STRIDE;
    float x0 = pieces[base + 0u] * scale;
    float y0 = pieces[base + 1u] * scale;
    float x1 = pieces[base + 2u] * scale;
    float y1 = pieces[base + 3u] * scale;
    float x2 = pieces[base + 4u] * scale;
    float y2 = pieces[base + 5u] * scale;
    Coefficients out;
    out.ax = x0;
    out.bx = 2.0f * (x1 - x0);
    out.cx = (x2 - x1) - (x1 - x0);
    out.ay = y0;
    out.by = 2.0f * (y1 - y0);
    out.cy = (y2 - y1) - (y1 - y0);
    return out;
}

static float eval_x(Coefficients coefficients, float t) {
    return coefficients.ax + coefficients.bx * t + coefficients.cx * t * t;
}

static float eval_y(Coefficients coefficients, float t) {
    return coefficients.ay + coefficients.by * t + coefficients.cy * t * t;
}

static float invert_component(
    float a,
    float b,
    float c,
    float target,
    float t_lo,
    float t_hi,
    float value_lo,
    float value_hi
) {
    bool ascending = value_hi >= value_lo;
    float minimum = fmin_(value_lo, value_hi);
    float maximum = fmax_(value_lo, value_hi);
    if (target <= minimum) return ascending ? t_lo : t_hi;
    if (target >= maximum) return ascending ? t_hi : t_lo;
    float roots[2];
    int count = solve_quadratic(c, b, a - target, roots);
    for (int index = 0; index < count; index++) {
        if (roots[index] >= t_lo && roots[index] <= t_hi) return roots[index];
    }
    return t_lo
        + (t_hi - t_lo) * ((target - value_lo) / (value_hi - value_lo));
}

static float t_at_x(
    Coefficients coefficients,
    float target,
    float t_lo,
    float t_hi
) {
    return invert_component(
        coefficients.ax,
        coefficients.bx,
        coefficients.cx,
        target,
        t_lo,
        t_hi,
        eval_x(coefficients, t_lo),
        eval_x(coefficients, t_hi)
    );
}

static float t_at_y(
    Coefficients coefficients,
    float target,
    float t_lo,
    float t_hi
) {
    return invert_component(
        coefficients.ay,
        coefficients.by,
        coefficients.cy,
        target,
        t_lo,
        t_hi,
        eval_y(coefficients, t_lo),
        eval_y(coefficients, t_hi)
    );
}

static void deposit(
    thread float *cell,
    thread float *spill,
    thread float *carry,
    uint x_lo,
    float x_a,
    float x_b,
    float y_a,
    float y_b
) {
    float delta = y_b - y_a;
    if (delta == 0.0f) return;
    float midpoint = 0.5f * (x_a + x_b);
    float floored = floor(midpoint);
    int column = (int)floored;
    if (column < (int)x_lo) {
        *carry += delta;
        return;
    }
    if (column > (int)x_lo) return;
    float fraction = midpoint - floored;
    *cell += delta * (1.0f - fraction);
    *spill += delta * fraction;
}

// Deposit one monotone piece into a one-cell scanline window.
static void accumulate_piece_cell(
    Coefficients coefficients,
    uint row_y,
    uint cell_x,
    thread float *cell,
    thread float *carry
) {
    float row = (float)row_y;
    float row_end = row + 1.0f;
    float y0 = eval_y(coefficients, 0.0f);
    float y1 = eval_y(coefficients, 1.0f);
    float band_lo = fmax_(fmin_(y0, y1), row);
    float band_hi = fmin_(fmax_(y0, y1), row_end);
    if (band_hi <= band_lo) return;
    float u = t_at_y(coefficients, band_lo, 0.0f, 1.0f);
    float v = t_at_y(coefficients, band_hi, 0.0f, 1.0f);
    float t_a = fmin_(u, v);
    float t_b = fmax_(u, v);
    if (t_b <= t_a) return;

    float x_a = eval_x(coefficients, t_a);
    float x_b = eval_x(coefficients, t_b);
    bool increasing = x_b >= x_a;
    float left = (float)cell_x;
    float right = left + 1.0f;
    float t_left = t_at_x(coefficients, left, t_a, t_b);
    float t_right = t_at_x(coefficients, right, t_a, t_b);

    float left_start = increasing ? t_a : t_left;
    float left_end = increasing ? t_left : t_b;
    if (left_end > left_start) {
        *carry += eval_y(coefficients, left_end) - eval_y(coefficients, left_start);
    }

    float t_previous = increasing ? t_left : t_right;
    float t_end = increasing ? t_right : t_left;
    if (t_end <= t_previous) return;
    float x_previous = clamp(eval_x(coefficients, t_previous), left, right);
    float y_previous = eval_y(coefficients, t_previous);
    float x_end = clamp(eval_x(coefficients, t_end), left, right);
    float y_end = eval_y(coefficients, t_end);

    float spill = 0.0f;
    // A one-cell window crosses at most one interior integer boundary. The
    // bounded loop retains the production fill's total-progress guarantee.
    for (uint step = 0u; step < 3u; step++) {
        float boundary = increasing ? floor(x_previous) + 1.0f
                                    : ceil(x_previous) - 1.0f;
        bool past = increasing ? boundary >= x_end : boundary <= x_end;
        float t_next;
        float x_next;
        if (past) {
            t_next = t_end;
            x_next = x_end;
        } else {
            t_next = t_at_x(coefficients, boundary, t_previous, t_end);
            x_next = boundary;
        }
        if (t_next <= t_previous && !past) {
            float denominator = x_end - x_previous;
            if (denominator != 0.0f) {
                t_next = t_previous
                    + (t_end - t_previous) * ((boundary - x_previous) / denominator);
            }
        }
        if (t_next <= t_previous) {
            t_next = t_end;
            x_next = x_end;
        }
        float y_next = (t_next >= t_end) ? y_end : eval_y(coefficients, t_next);
        deposit(
            cell,
            &spill,
            carry,
            cell_x,
            x_previous,
            x_next,
            y_previous,
            y_next
        );
        if (t_next >= t_end) break;
        t_previous = t_next;
        x_previous = x_next;
        y_previous = y_next;
    }
}

static float fill_coverage(
    device const float *pieces,
    uint first,
    uint count,
    uint row_y,
    uint cell_x,
    uint samples,
    uint sample_x,
    uint sample_y
) {
    uint high_x = cell_x * samples + sample_x;
    uint high_y = row_y * samples + sample_y;
    float scale = (float)samples;
    float accumulated = 0.0f;
    for (uint index = first; index < first + count; index++) {
        float cell = 0.0f;
        float carry = 0.0f;
        accumulate_piece_cell(
            piece_coefficients(pieces, index, scale),
            high_y,
            high_x,
            &cell,
            &carry
        );
        accumulated += carry + cell;
    }
    return min(fabs(accumulated), 1.0f);
}

static float2 station_delta(
    device const float *stations,
    uint index,
    float2 point
) {
    uint base = index * STATION_STRIDE;
    return float2(stations[base], stations[base + 1u]) - point;
}

// Returns false exactly for the on-edge limit where tan(alpha/2) diverges.
static bool station_tan_half(
    device const float *stations,
    uint a,
    uint b,
    float2 point,
    thread float *value
) {
    float2 da = station_delta(stations, a, point);
    float2 db = station_delta(stations, b, point);
    float ra = length(da);
    float rb = length(db);
    float product = ra * rb;
    float dotted = dot(da, db);
    float crossed = da.x * db.y - da.y * db.x;
    if (fabs(crossed) <= DEGENERATE_REL * product) {
        if (dotted < 0.0f) return false;
        *value = 0.0f;
        return true;
    }
    *value = (product - dotted) / crossed;
    return true;
}

static float nearest_station_parameter(
    device const float *stations,
    uint first,
    uint count,
    float2 point
) {
    float best = INFINITY;
    float parameter = 0.0f;
    for (uint offset = 0u; offset < count; offset++) {
        uint index = first + offset;
        float2 delta = station_delta(stations, index, point);
        float distance_squared = dot(delta, delta);
        if (distance_squared < best) {
            best = distance_squared;
            parameter = stations[index * STATION_STRIDE + 2u];
        }
    }
    return parameter;
}

static float fill_parameter(
    device const float *stations,
    uint first,
    uint count,
    float2 point
) {
    if (count == 0u) return 0.0f;
    if (count == 1u) return stations[first * STATION_STRIDE + 2u];

    for (uint offset = 0u; offset < count; offset++) {
        uint index = first + offset;
        float2 delta = station_delta(stations, index, point);
        if (dot(delta, delta) <= 1e-10f) {
            return stations[index * STATION_STRIDE + 2u];
        }
    }
    for (uint offset = 0u; offset < count; offset++) {
        uint a = first + offset;
        uint b = first + ((offset + 1u) % count);
        float ignored = 0.0f;
        if (!station_tan_half(stations, a, b, point, &ignored)) {
            float ra = length(station_delta(stations, a, point));
            float rb = length(station_delta(stations, b, point));
            float fraction = (ra + rb > 0.0f) ? ra / (ra + rb) : 0.0f;
            float pa = stations[a * STATION_STRIDE + 2u];
            float pb = (offset + 1u == count)
                ? 1.0f
                : stations[b * STATION_STRIDE + 2u];
            return mix(pa, pb, fraction);
        }
    }

    float wrap = 0.0f;
    station_tan_half(
        stations,
        first + count - 1u,
        first,
        point,
        &wrap
    );
    float previous = wrap;
    float numerator = 0.0f;
    float denominator = 0.0f;
    for (uint offset = 0u; offset < count; offset++) {
        uint index = first + offset;
        float current = wrap;
        if (offset + 1u < count) {
            station_tan_half(stations, index, index + 1u, point, &current);
        }
        float radius = length(station_delta(stations, index, point));
        if (radius > 0.0f) {
            float weight = (previous + current) / radius;
            numerator += weight * stations[index * STATION_STRIDE + 2u];
            denominator += weight;
        }
        previous = current;
    }
    if (
        denominator == 0.0f
        || !isfinite(denominator)
        || !isfinite(numerator)
    ) {
        return nearest_station_parameter(stations, first, count, point);
    }
    return clamp(numerator / denominator, 0.0f, 1.0f);
}

static float aa_coverage(float excess, float aa_width) {
    float width = max(aa_width, MIN_AA_WIDTH);
    float t = clamp(0.5f - excess / width, 0.0f, 1.0f);
    return t * t * (3.0f - 2.0f * t);
}

static float4 source_over(float4 source, float4 destination) {
    float source_alpha = source.a;
    if (source_alpha <= 0.0f) return destination;
    float output_alpha = source_alpha + destination.a * (1.0f - source_alpha);
    if (output_alpha <= 0.0f) return float4(0.0f);
    float3 rgb = (
        source.rgb * source_alpha
        + destination.rgb * destination.a * (1.0f - source_alpha)
    ) / output_alpha;
    return float4(rgb, output_alpha);
}

static float4 fill_ramp(device const float *styles, uint style_base, float t) {
    float4 start = float4(
        styles[style_base + 0u],
        styles[style_base + 1u],
        styles[style_base + 2u],
        styles[style_base + 3u]
    );
    float4 end = float4(
        styles[style_base + 4u],
        styles[style_base + 5u],
        styles[style_base + 6u],
        styles[style_base + 7u]
    );
    return mix(start, end, clamp(t, 0.0f, 1.0f));
}

static float4 stroke_ramp(device const float *styles, uint style_base, float t) {
    float4 start = float4(
        styles[style_base + 8u],
        styles[style_base + 9u],
        styles[style_base + 10u],
        styles[style_base + 11u]
    );
    float4 end = float4(
        styles[style_base + 12u],
        styles[style_base + 13u],
        styles[style_base + 14u],
        styles[style_base + 15u]
    );
    return mix(start, end, clamp(t, 0.0f, 1.0f));
}

static float2 nearest_boundary(
    device const float *segments,
    uint first,
    uint count,
    float2 point
) {
    float best_distance = INFINITY;
    float best_parameter = 0.0f;
    for (uint index = first; index < first + count; index++) {
        uint base = index * SEGMENT_STRIDE;
        float2 p0 = float2(segments[base], segments[base + 1u]);
        float2 p1 = float2(segments[base + 2u], segments[base + 3u]);
        float2 p2 = float2(segments[base + 4u], segments[base + 5u]);
        float2 nearest = (segments[base + SEGMENT_LINEAR] != 0.0f)
            ? distance_to_capsule(point, p0, p2)
            : distance_to_quadratic(point, p0, p1, p2);
        if (nearest.x < best_distance) {
            best_distance = nearest.x;
            best_parameter = segment_path_parameter(segments, index, nearest.y);
        }
    }
    return float2(best_distance, best_parameter);
}

static float4 fill_colour(
    device const float *segments,
    device const float *stations,
    device const uint *draw_u32,
    device const float *styles,
    uint draw,
    float2 point
) {
    uint du = draw * DRAW_U32_STRIDE;
    uint style = draw * STYLE_STRIDE;
    uint flags = draw_u32[du + 8u];
    if ((flags & FLAT_FILL) != 0u) return fill_ramp(styles, style, 0.0f);

    uint first_station = draw_u32[du + 6u];
    uint station_count = draw_u32[du + 7u];
    float parameter =
        fill_parameter(stations, first_station, station_count, point);
    float4 interior = fill_ramp(styles, style, parameter);
    float border_width = styles[style + 18u];
    if (border_width <= 0.0f) return interior;

    uint first_segment = draw_u32[du + 0u];
    uint segment_count = draw_u32[du + 1u];
    float2 nearest =
        nearest_boundary(segments, first_segment, segment_count, point);
    if (!isfinite(nearest.x)) return interior;
    float coverage =
        aa_coverage(nearest.x - border_width, styles[style + 19u]);
    if (coverage <= 0.0f) return interior;
    return mix(interior, fill_ramp(styles, style, nearest.y), coverage);
}

static bool join_contains(
    device const float *joins,
    uint join,
    float2 point
) {
    uint base = join * JOIN_STRIDE;
    float2 delta = point - float2(joins[base], joins[base + 1u]);
    float2 incoming = float2(joins[base + 2u], joins[base + 3u]);
    float2 outgoing = float2(joins[base + 4u], joins[base + 5u]);
    return dot(delta, incoming) >= 0.0f && dot(delta, outgoing) <= 0.0f;
}

static float apply_joins(
    device const float *joins,
    uint first,
    uint count,
    uint joint,
    float2 point,
    float round_excess
) {
    float excess = round_excess;
    for (uint index = first; index < first + count; index++) {
        if (!join_contains(joins, index, point)) continue;
        uint base = index * JOIN_STRIDE;
        float2 anchor = float2(joins[base], joins[base + 1u]);
        float half_width = joins[base + 6u];
        float2 bisector = float2(joins[base + 7u], joins[base + 8u]);
        float2 normal_in = float2(joins[base + 9u], joins[base + 10u]);
        float2 normal_out = float2(joins[base + 11u], joins[base + 12u]);
        float2 delta = point - anchor;
        float bevel =
            dot(delta, bisector) - half_width * dot(normal_in, bisector);
        if (joint == 1u) {
            excess = max(excess, bevel);
        } else if (joint == 2u) {
            float cosine = dot(normal_in, bisector);
            float ratio = (cosine <= 0.0f) ? INFINITY : 1.0f / cosine;
            if (ratio > MITER_LIMIT) {
                excess = max(excess, bevel);
            } else {
                float miter = max(
                    dot(delta, normal_in) - half_width,
                    dot(delta, normal_out) - half_width
                );
                excess = min(excess, miter);
            }
        }
    }
    return excess;
}

static float2 stroke_shade(
    device const float *segments,
    device const float *joins,
    device const uint *draw_u32,
    device const float *styles,
    uint draw,
    float2 point
) {
    uint du = draw * DRAW_U32_STRIDE;
    uint style = draw * STYLE_STRIDE;
    uint first_segment = draw_u32[du + 0u];
    uint segment_count = draw_u32[du + 1u];
    float width_start = styles[style + 16u];
    float width_end = styles[style + 17u];
    float best_excess = INFINITY;
    float best_parameter = 0.0f;
    for (
        uint index = first_segment;
        index < first_segment + segment_count;
        index++
    ) {
        uint base = index * SEGMENT_STRIDE;
        float2 p0 = float2(segments[base], segments[base + 1u]);
        float2 p1 = float2(segments[base + 2u], segments[base + 3u]);
        float2 p2 = float2(segments[base + 4u], segments[base + 5u]);
        float2 nearest = (segments[base + SEGMENT_LINEAR] != 0.0f)
            ? distance_to_capsule(point, p0, p2)
            : distance_to_quadratic(point, p0, p1, p2);
        float parameter =
            segment_path_parameter(segments, index, nearest.y);
        float half_width = 0.5f * mix(width_start, width_end, parameter);
        float excess = nearest.x - half_width;
        if (excess < best_excess) {
            best_excess = excess;
            best_parameter = parameter;
        }
    }
    uint first_join = draw_u32[du + 4u];
    uint join_count = draw_u32[du + 5u];
    uint joint = draw_u32[du + 9u];
    best_excess = apply_joins(
        joins,
        first_join,
        join_count,
        joint,
        point,
        best_excess
    );
    return float2(
        aa_coverage(best_excess, styles[style + 19u]),
        best_parameter
    );
}

static float4 composite_fill(
    device const float *segments,
    device const float *pieces,
    device const float *stations,
    device const uint *draw_u32,
    device const float *draw_f32,
    device const float *styles,
    uint draw,
    uint command_flag,
    uint px,
    uint py,
    uint samples,
    uint sample_x,
    uint sample_y,
    float2 point,
    float4 destination
) {
    uint du = draw * DRAW_U32_STRIDE;
    uint df = draw * DRAW_F32_STRIDE;
    uint flags = draw_u32[du + 8u];
    if ((flags & DRAW_FILL) == 0u) return destination;
    if (
        point.x < draw_f32[df + 0u]
        || point.x > draw_f32[df + 2u]
        || point.y < draw_f32[df + 1u]
        || point.y > draw_f32[df + 3u]
    ) {
        return destination;
    }
    float coverage = (command_flag == CLASS_INTERIOR)
        ? 1.0f
        : fill_coverage(
            pieces,
            draw_u32[du + 2u],
            draw_u32[du + 3u],
            py,
            px,
            samples,
            sample_x,
            sample_y
        );
    if (coverage <= 0.0f) return destination;
    float4 colour =
        fill_colour(segments, stations, draw_u32, styles, draw, point);
    colour.a *= coverage;
    return source_over(colour, destination);
}

static float4 composite_stroke(
    device const float *segments,
    device const float *joins,
    device const uint *draw_u32,
    device const float *draw_f32,
    device const float *styles,
    uint draw,
    float2 point,
    float4 destination
) {
    uint du = draw * DRAW_U32_STRIDE;
    uint df = draw * DRAW_F32_STRIDE;
    uint flags = draw_u32[du + 8u];
    if ((flags & DRAW_STROKE) == 0u) return destination;
    if (
        point.x < draw_f32[df + 4u]
        || point.x > draw_f32[df + 6u]
        || point.y < draw_f32[df + 5u]
        || point.y > draw_f32[df + 7u]
    ) {
        return destination;
    }
    float2 shaded =
        stroke_shade(segments, joins, draw_u32, styles, draw, point);
    if (shaded.x <= 0.0f) return destination;
    float4 colour = stroke_ramp(styles, draw * STYLE_STRIDE, shaded.y);
    colour.a *= shaded.x;
    return source_over(colour, destination);
}

static float4 render_sample(
    device const float *segments,
    device const float *pieces,
    device const float *joins,
    device const float *stations,
    device const uint *draw_u32,
    device const float *draw_f32,
    device const float *styles,
    device const uint *tile_offsets,
    device const uint *tile_draws,
    device const uint *tile_flags,
    uint draw_count,
    uint tile_index,
    uint px,
    uint py,
    uint samples,
    uint sample_x,
    uint sample_y,
    float4 background
) {
    float inverse = 1.0f / (float)samples;
    float2 point = float2(
        (float)px + ((float)sample_x + 0.5f) * inverse,
        (float)py + ((float)sample_y + 0.5f) * inverse
    );
    float4 accumulated = background;
    uint lo = tile_offsets[tile_index];
    uint hi = tile_offsets[tile_index + 1u];
    for (uint command = lo; command < hi; command++) {
        uint draw = tile_draws[command];
        if (draw >= draw_count) continue;
        uint flags = draw_u32[draw * DRAW_U32_STRIDE + 8u];
        if ((flags & STROKE_BEHIND) != 0u) {
            accumulated = composite_stroke(
                segments,
                joins,
                draw_u32,
                draw_f32,
                styles,
                draw,
                point,
                accumulated
            );
            accumulated = composite_fill(
                segments,
                pieces,
                stations,
                draw_u32,
                draw_f32,
                styles,
                draw,
                tile_flags[command],
                px,
                py,
                samples,
                sample_x,
                sample_y,
                point,
                accumulated
            );
        } else {
            accumulated = composite_fill(
                segments,
                pieces,
                stations,
                draw_u32,
                draw_f32,
                styles,
                draw,
                tile_flags[command],
                px,
                py,
                samples,
                sample_x,
                sample_y,
                point,
                accumulated
            );
            accumulated = composite_stroke(
                segments,
                joins,
                draw_u32,
                draw_f32,
                styles,
                draw,
                point,
                accumulated
            );
        }
    }
    return accumulated;
}

kernel void fmn_render_frame(
    constant uint *params_u32 [[buffer(0)]],
    constant float *params_f32 [[buffer(1)]],
    device const float *segments [[buffer(2)]],
    device const float *pieces [[buffer(3)]],
    device const float *joins [[buffer(4)]],
    device const float *stations [[buffer(5)]],
    device const uint *draw_u32 [[buffer(6)]],
    device const float *draw_f32 [[buffer(7)]],
    device const float *styles [[buffer(8)]],
    device const uint *tile_offsets [[buffer(9)]],
    device const uint *tile_draws [[buffer(10)]],
    device const uint *tile_flags [[buffer(11)]],
    device half4 *surface [[buffer(12)]],
    device uint *status [[buffer(13)]],
    uint2 group_id [[threadgroup_position_in_grid]],
    uint2 local_id [[thread_position_in_threadgroup]]
) {
    uint width = params_u32[0];
    uint height = params_u32[1];
    uint tile = params_u32[2];
    uint cols = params_u32[3];
    uint samples = params_u32[4];
    uint draw_count = params_u32[5];
    uint px = group_id.x * tile + local_id.x;
    uint py = group_id.y * tile + local_id.y;
    bool active = px < width && py < height;

    if (active) {
        uint tile_index = group_id.y * cols + group_id.x;
        float4 background =
            float4(params_f32[0], params_f32[1], params_f32[2], params_f32[3]);
        float4 premultiplied_sum = float4(0.0f);
        for (uint sample_y = 0u; sample_y < samples; sample_y++) {
            for (uint sample_x = 0u; sample_x < samples; sample_x++) {
                float4 sample = render_sample(
                    segments,
                    pieces,
                    joins,
                    stations,
                    draw_u32,
                    draw_f32,
                    styles,
                    tile_offsets,
                    tile_draws,
                    tile_flags,
                    draw_count,
                    tile_index,
                    px,
                    py,
                    samples,
                    sample_x,
                    sample_y,
                    background
                );
                premultiplied_sum.rgb += sample.rgb * sample.a;
                premultiplied_sum.a += sample.a;
            }
        }
        float count = (float)(samples * samples);
        float alpha = premultiplied_sum.a / count;
        float3 rgb = (premultiplied_sum.a > 0.0f)
            ? premultiplied_sum.rgb / premultiplied_sum.a
            : float3(0.0f);
        surface[py * width + px] = half4(half3(rgb), half(alpha));
    }

    // Every lane reaches the barrier, including padding lanes at edge tiles.
    // A fresh status buffer therefore distinguishes a completed dispatch from
    // the pinned gateway returning after a command-buffer runtime failure.
    threadgroup_barrier(mem_flags::mem_device);
    if (local_id.x == 0u && local_id.y == 0u) {
        status[group_id.y * params_u32[3] + group_id.x] = STATUS_COMPLETE;
    }
}

static uchar quantize_unit(float value) {
    return (uchar)clamp(round(clamp(value, 0.0f, 1.0f) * 255.0f), 0.0f, 255.0f);
}

static float srgb_encode(float linear) {
    float value = clamp(linear, 0.0f, 1.0f);
    return (value <= 0.0031308f)
        ? 12.92f * value
        : 1.055f * pow(value, 1.0f / 2.4f) - 0.055f;
}

kernel void fmn_rgba16f_to_rgba8(
    constant uint *params [[buffer(0)]], // width, height, stride
    device const half4 *source [[buffer(1)]],
    device uchar *output [[buffer(2)]],
    device uint *status [[buffer(3)]],
    uint2 group_id [[threadgroup_position_in_grid]],
    uint2 local_id [[thread_position_in_threadgroup]],
    uint2 group_size [[threads_per_threadgroup]]
) {
    uint px = group_id.x * group_size.x + local_id.x;
    uint py = group_id.y * group_size.y + local_id.y;
    if (px < params[0] && py < params[1]) {
        float4 value = float4(source[py * params[0] + px]);
        uint base = py * params[2] + px * 4u;
        output[base + 0u] = quantize_unit(srgb_encode(value.r));
        output[base + 1u] = quantize_unit(srgb_encode(value.g));
        output[base + 2u] = quantize_unit(srgb_encode(value.b));
        output[base + 3u] = quantize_unit(value.a);
    }
    threadgroup_barrier(mem_flags::mem_device);
    if (local_id.x == 0u && local_id.y == 0u) {
        uint groups_x = (params[0] + group_size.x - 1u) / group_size.x;
        status[group_id.y * groups_x + group_id.x] = STATUS_COMPLETE;
    }
}
