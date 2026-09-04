from pathlib import Path

path = Path("crates/fmn-python/src/lib.rs")
source = path.read_text(encoding="utf-8")

old_bootstrap = '''    let source = CString::new(include_str!("../python/manimlib_bootstrap.py"))
        .expect("embedded bootstrap contains no NUL");
    let globals = module.dict();
    let result = py.run(source.as_c_str(), Some(&globals), Some(&globals));
    module.delattr("_FMN_MODULE")?;
    result
'''
new_bootstrap = '''    let bootstrap_source = CString::new(include_str!("../python/manimlib_bootstrap.py"))
        .expect("embedded bootstrap contains no NUL");
    let animation_source = CString::new(include_str!("../python/animation_semantics.py"))
        .expect("embedded animation semantics contain no NUL");
    let globals = module.dict();
    let result: PyResult<()> = (|| {
        py.run(bootstrap_source.as_c_str(), Some(&globals), Some(&globals))?;
        py.run(animation_source.as_c_str(), Some(&globals), Some(&globals))?;
        Ok(())
    })();
    module.delattr("_FMN_MODULE")?;
    result
'''
if source.count(old_bootstrap) != 1:
    raise SystemExit("execute_bootstrap anchor drifted")
source = source.replace(old_bootstrap, new_bootstrap, 1)

old_test = '''            let source = CString::new(include_str!("../tests/bridge.py"))
                .expect("test source contains no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .expect("Python bridge acceptance suite");
'''
new_test = old_test + '''            globals
                .set_item(
                    "__file__",
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/animation_semantics.py"
                    ),
                )
                .expect("set animation semantic suite source path");
            let animation_source =
                CString::new(include_str!("../tests/animation_semantics.py"))
                    .expect("animation semantic test source contains no NUL");
            py.run(animation_source.as_c_str(), Some(globals), Some(globals))
                .expect("Python Animation semantic acceptance suite");
'''
if source.count(old_test) != 1:
    raise SystemExit("bridge acceptance anchor drifted")
source = source.replace(old_test, new_test, 1)
path.write_text(source, encoding="utf-8")
