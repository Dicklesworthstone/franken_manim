"""Integrate the Studio input identity through existing native ownership seams.

An exact, idempotent delivery edit: no generated API rows, dependency pins,
release claims or unrelated source are rewritten.
"""
from pathlib import Path


def edit(name, old, new):
    path = Path(name)
    text = path.read_text()
    if new in text:
        return
    if text.count(old) != 1:
        raise RuntimeError(f"refusing drifted integration target: {name}: {old[:90]!r}")
    path.write_text(text.replace(old, new, 1))


watch = "crates/fmn-studio/src/project_watch.rs"
edit(watch, "    /// Return true once a different content set has remained stable for the", '''    /// Content identity of the last complete observed scan. This is independent
    /// of debounce state and binds paths, entry kinds and file content, not
    /// mtimes. Construct a fresh watcher for a launch/publication snapshot.
    /// Path encodings are host-local; this is not a certified portable closure.
    #[must_use]
    pub fn content_fingerprint(&self) -> ProtocolDigest {
        let mut hash = fmn_hash::Sha256::new();
        hash.update(b"FMN-SOURCE-WATCH\\0\\x01");
        for (path, input) in &self.observed {
            let bytes = path.as_os_str().as_encoded_bytes();
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
            match input {
                Input::Missing => hash.update(&[0]),
                Input::Directory => hash.update(&[1]),
                Input::File(digest) => {
                    hash.update(&[2]);
                    hash.update(digest.as_bytes());
                }
            }
        }
        hash.finalize()
    }

    /// Borrow the file rows from that same observed scan, in path order.
    /// Consumers can compare actually compiled source bytes without rescanning
    /// or inventing another directory traversal/filtering policy.
    pub fn source_files(&self) -> impl Iterator<Item = (&std::path::Path, ProtocolDigest)> {
        self.observed.iter().filter_map(|(path, input)| match input {
            Input::File(digest) => Some((path.as_path(), *digest)),
            _ => None,
        })
    }

    /// Return true once a different content set has remained stable for the''')
portal = "crates/fmn-python/src/portal_studio.rs"
edit(portal, "impl SourceWatcher {\n    fn poll", '''impl SourceWatcher {
    #[getter]
    fn fingerprint(&self) -> String {
        self.watch.content_fingerprint().to_hex()
    }

    #[getter]
    fn files(&self) -> PyResult<Vec<(String, String)>> {
        self.watch
            .source_files()
            .map(|(path, digest)| {
                let path = path.to_str().ok_or_else(|| {
                    PyValueError::new_err("Studio source identity requires UTF-8 paths")
                })?;
                Ok((path.to_owned(), digest.to_hex()))
            })
            .collect()
    }

    fn poll''')
portal_py = "crates/fmn-python/python/fmn_python/studio.py"
edit(portal_py, "from . import _ensure_exclusive_manimlib_namespace", "from . import _ensure_exclusive_manimlib_namespace\nfrom .studio_inputs import StudioInputs, project_directory")
edit(portal_py, "roots = list(dict.fromkeys([str(path), str(path.parent),", "roots = list(dict.fromkeys([str(path), str(project_directory(path)),")
edit(portal_py, '"portal": _file_digest(Path(__file__).resolve()),\n                   "abi"', '"portal": _file_digest(Path(__file__).resolve()),\n                   "inputs": _file_digest(Path(sys.modules[StudioInputs.__module__].__file__).resolve()),\n                   "abi"')
edit(portal_py, '"portal": _file_digest(Path(__file__).resolve()),\n                  "abi"', '"portal": _file_digest(Path(__file__).resolve()),\n                  "inputs": _file_digest(Path(sys.modules[StudioInputs.__module__].__file__).resolve()),\n                  "abi"')
edit(portal_py, '''        def rebuild():
            data = _source(path)
            request = {"schema": _SCHEMA, "version": 1, "source": str(path), "scene": scene,
                       "source_sha256": hashlib.sha256(data).hexdigest(),
                       "runtime": runtime, **options}''', '''        def rebuild():
            data = _source(path)
            inputs = StudioInputs(_native, roots)
            source_sha256 = hashlib.sha256(data).hexdigest()
            inputs.require_source(path, source_sha256)
            request = {"schema": _SCHEMA, "version": 2, "source": str(path), "scene": scene,
                       "source_sha256": source_sha256, "inputs": inputs.as_request(),
                       "runtime": runtime, **options}''')
edit(portal_py, '''    path = Path(request["source"])
    if hashlib.sha256(_source(path)).hexdigest() != request["source_sha256"]:''', '''    path = Path(request["source"])
    inputs = StudioInputs.from_request(native, request.get("inputs"))
    inputs.require_source(path, request["source_sha256"])
    if hashlib.sha256(_source(path)).hexdigest() != request["source_sha256"]:''')
edit(portal_py, '''        if request["scene"] not in loaded.scenes:''', '''        inputs.verify(loaded)
        if request["scene"] not in loaded.scenes:''')
edit(portal_py, '''        scene = loaded.scenes[request["scene"]]()
        # No source rewrite''', '''        scene = loaded.scenes[request["scene"]]()
        inputs.verify(loaded)
        # No source rewrite''')
edit(portal_py, '''            request["scene"], request["build_id"], request["source_sha256"],''', '''            request["scene"], request["build_id"], inputs.fingerprint,''')
edit(portal_py, '''            if request.get("interactive", False):''', '''            inputs.verify(loaded)
            if request.get("interactive", False):''')
edit(portal_py, '''            return scene._finish_studio_capture()
        except BaseException:
            scene._abort_render()
            raise''', '''            recording = scene._finish_studio_capture()
            # Finishing an empty scene can capture a frame and invoke authored
            # camera/updater hooks. Validate after those effects, before serve.
            inputs.verify(loaded)
            return recording
        except BaseException as error:
            try:
                scene._abort_render()
            except BaseException as cleanup:
                error.add_note("Studio capture cleanup also failed: " + type(cleanup).__name__)
            raise''')
edit(portal_py, 'request.get("version") != 1', 'request.get("version") != 2')
edit(portal_py, '''--autoreload explicitly reruns source on content changes to local .py/.pyw
files; --watch adds directories or explicit asset files. Native bounded scans''', '''Launch and publication bind the containing package's .py/.pyw inputs and
explicitly watched assets to the native capture identity. Changed or undeclared
compiled project inputs refuse publication, rather than serving a stale build.
--autoreload explicitly reruns source on content changes in that package;
--watch adds directories or explicit asset files. Native bounded scans''')
tests = "crates/fmn-python/tests/test_studio_protocol.py"
edit(tests, '        return SimpleNamespace(poll=lambda: False, watched=(roots, debounce_ms))', '''        from test_studio_inputs_protocol import fake_snapshot
        return fake_snapshot(roots, debounce_ms)''')
edit(tests, 'self.assertEqual(events, ["snapshot", "worker"])', 'self.assertEqual(events, ["snapshot", "worker", "snapshot"])')
