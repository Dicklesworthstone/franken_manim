"""Apply the reviewed CLI offload tranche only to its exact source parents.

The publication workflow compiles/tests the resulting clean candidate commit
before advancing main. This script never invokes Git or changes the work graph.
"""
from pathlib import Path
import hashlib

ROOT = Path(__file__).resolve().parents[3]
PAYLOAD = Path(__file__).resolve().parent
EXPECTED = {
    'crates/fmn-cli/src/lib.rs': '5a4c2404a873a3a037953eda3826398c68bef3d2',
    'crates/fmn-cli/src/offline_cpu.rs': 'ff985d5f858000fa921f3cd2a1a8e4871235ed0b',
    'crates/fmn-conformance/tests/e2e_scenarios.rs': '9fe659282b3a60d070e4f97acf8c3907da62f15c',
}
NEW = {
    'module.rs': 'crates/fmn-cli/src/offline_cpu/pure.rs',
    'module_tests.rs': 'crates/fmn-cli/src/offline_cpu/pure/tests.rs',
    'cli_tests.rs': 'crates/fmn-cli/tests/pure_segments.rs',
    'guide.md': 'docs/CLI_PURE_SEGMENTS.md',
}
for name, expected in EXPECTED.items():
    data = (ROOT / name).read_bytes()
    actual = hashlib.sha1(f'blob {len(data)}\0'.encode() + data).hexdigest()
    if actual != expected:
        raise SystemExit(f'refusing changed source parent: {name}: {actual}')
for source, destination in NEW.items():
    if (ROOT / destination).exists() or (ROOT / destination).is_symlink():
        raise SystemExit(f'refusing existing destination: {destination}')
    if not (PAYLOAD / source).is_file():
        raise SystemExit(f'missing reviewed payload: {source}')

pending = {}
def edit(path, substitutions):
    text = (ROOT / path).read_text()
    for old, new, count in substitutions:
        if text.count(old) != count:
            raise SystemExit(f'refusing changed insertion context in {path}: {old[:80]!r}')
        text = text.replace(old, new)
    pending[path] = text

edit('crates/fmn-cli/src/lib.rs', [
 ('    pipeline: Option<fmn_runtime::PipelineStats>,\n','    pipeline: Option<fmn_runtime::PipelineStats>,\n    dispatch: offline_cpu::DispatchStats,\n',2),
 ('            pipeline: None,\n','            pipeline: None,\n            dispatch: offline_cpu::DispatchStats::default(),\n',2),
 ('        pipeline: None,\n    }\n}\n','        pipeline: None,\n        dispatch: offline_cpu::DispatchStats::default(),\n    }\n}\n',1),
 ('            pipeline: self.pipeline.clone(),\n','            pipeline: self.pipeline.clone(),\n            dispatch: self.dispatch,\n',1),
 ('    next_sequence: u64,\n}\n\nfn resolved_frame_config','    next_sequence: u64,\n    pure_enabled: bool,\n    failure: Option<IntegrationError>,\n}\n\nfn resolved_frame_config',1),
 ('            next_sequence: 0,\n        })\n    }\n\n    fn render_stage','            next_sequence: 0,\n            pure_enabled: !matches!(target, RenderTarget::Native(NativeFrameFormat::Png)),\n            failure: None,\n        })\n    }\n\n    fn render_stage',1),
 ('    fn finish(mut self, audio: Option<RenderSoundtrack<\'_>>) -> Result<FinishedRender, CliError> {\n','    fn finish(mut self, audio: Option<RenderSoundtrack<\'_>>) -> Result<FinishedRender, CliError> {\n        if let Some(error) = &self.failure {\n            return Err(CliError::new("render", error.to_string()));\n        }\n        self.backend.dispatch.validate(self.backend.frames)?;\n',1),
 ('impl SceneSink for RenderSink {\n    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {\n','impl SceneSink for RenderSink {\n    fn accepts_pure_segments(&self) -> bool {\n        self.accepts_native_programs()\n    }\n\n    fn capture_pure_segment(\n        &mut self,\n        segment: &fmn::animation::pure_frame::PureSegment,\n    ) -> Result<(), IntegrationError> {\n        self.render_pure(segment)\n    }\n\n    fn event(&mut self, event: LifecycleEvent) -> Result<(), IntegrationError> {\n        if let Some(error) = &self.failure {\n            return Err(error.clone());\n        }\n        if event.phase == LifecyclePhase::DriveSegment && !event.skipping {\n            self.backend.dispatch.begin_segment()?;\n        }\n',1),
 ('        let stage = packet.materialize_stage();\n        let revision = u64::try_from(packet.frame_index())\n            .map_err(|_| IntegrationError::new("lumen", "negative frame index"))?;\n        self.render_stage(&stage, revision)\n','        if let Some(error) = &self.failure {\n            return Err(error.clone());\n        }\n        let result = (|| {\n            let stage = packet.materialize_stage();\n            let revision = u64::try_from(packet.frame_index())\n                .map_err(|_| IntegrationError::new("lumen", "negative frame index"))?;\n            self.render_stage(&stage, revision)\n        })();\n        self.latch_capture(result)\n',1),
 ('    S: SceneSink + ?Sized,\n{\n    fn event','    S: SceneSink + ?Sized,\n{\n    fn accepts_pure_segments(&self) -> bool {\n        !self.cancellation.is_cancelled() && self.inner.accepts_pure_segments()\n    }\n\n    fn capture_pure_segment(\n        &mut self,\n        segment: &fmn::animation::pure_frame::PureSegment,\n    ) -> Result<(), IntegrationError> {\n        self.cancellation.scene_checkpoint()?;\n        // The registered emitter interrupts admission within a long program.\n        // Never return a fallback after any of its samples were consumed.\n        self.inner.capture_pure_segment(segment)?;\n        self.cancellation.scene_checkpoint()\n    }\n\n    fn event',1),
 ('        "determinism mode and declared execution configuration",\n','        format!(\n            "determinism mode and declared execution configuration; execution_dispatch={}",\n            backend.dispatch.to_json(),\n        ),\n',1),
 ('        if let Some(stats) = &backend.pipeline {\n            // Keep scheduler receipts','        backend_json.push_str(&format!(",\\\"execution_dispatch\\\":{}", backend.dispatch.to_json()));\n        if let Some(stats) = &backend.pipeline {\n            // Keep scheduler receipts',1),
])
edit('crates/fmn-cli/src/offline_cpu.rs', [
 ('use fmn::rendering::{NativeFramePipeline, RenderError};','mod pure;\npub(super) use pure::DispatchStats;\n\nuse fmn::rendering::{NativeFramePipeline, RenderError};',1),
 ('                self.backend.record_cpu_frame()?;\n                self.backend.route =','                self.backend.record_cpu_frame()?;\n                self.backend.dispatch.compiled_frame()?;\n                self.backend.route =',1),
])

path = 'crates/fmn-conformance/tests/e2e_scenarios.rs'
text = (ROOT / path).read_text()
if text.count('fmn::SceneError::') != 4:
    raise SystemExit('changed conformance SceneError insertion context')
text = text.replace('fmn::SceneError::', 'fmn::scene::SceneError::')
old = 'stage.arena_mut().add(fmn_library::triangle().radius(0.4))'
if text.count(old) != 1:
    raise SystemExit('changed conformance triangle insertion context')
text = text.replace(old, 'stage.arena_mut().add(fmn_library::triangle().radius(0.4).build()?)')
function = r'''
/// The production CLI, not an alternate renderer, consumes native programs.
fn cli_pure_dispatch_run(ctx: &mut RunCtx) -> Result<RunOutcome, ScenarioError> {
    let mut baseline = None;
    for threads in ["1", "4", "16"] {
        let (output, frames) = cli_builtin_frames("circle_shift.v1", threads)?;
        let dispatched = output.stdout.contains("\"pure_segments\":2")
            && output.stdout.contains("\"serial_segments\":0")
            && output.stdout.contains("\"pure_frames\":3")
            && output.stdout.contains("\"compiled_frames\":0")
            && output.stdout.contains("\"outstanding_slots\":0");
        if output.code != 0 || !dispatched || frames.len() != 3 {
            return Err(fail(format!("CLI pure dispatch {threads}: {:?} {:?}", output.stdout, output.stderr)));
        }
        if let Some(baseline) = &baseline {
            if &frames != baseline { return Err(fail("pure CLI artifacts changed with thread count")); }
        } else { baseline = Some(frames); }
        ctx.event(LogEvent::new("e2e.cli.pure_dispatch")
            .field("threads", threads).field("pure_segments", 2_u64).field("pure_frames", 3_u64));
    }
    Ok(RunOutcome::ok().with_counter("cli_pure_runs", 3).with_counter("cli_pure_frames", 9))
}

'''
anchor = '/// The CLI builtins\' determinism drill'
if text.count(anchor) != 1:
    raise SystemExit('changed Gauntlet scenario insertion context')
text = text.replace(anchor, function + anchor)
anchor = '    let mut specs = vec![\n'
if text.count(anchor) != 1:
    raise SystemExit('changed Gauntlet catalog insertion context')
text = text.replace(anchor, '''    let mut specs = vec![
        spec(
            "determinism.cli_pure_segments.v1",
            ScenarioClass::DeterminismDrill,
            Surface::CliInProcess,
            Invocation::new(cli_pure_dispatch_run),
            vec![Assertion::ExitCode(0), counter_eq("cli_pure_runs", 3), counter_eq("cli_pure_frames", 9)],
            ["1", "4", "16"].into_iter().map(|threads| LogExpect::span_present(
                "e2e.cli.pure_dispatch",
                vec![FieldPred::str_eq("threads", threads), FieldPred::u64_eq("pure_segments", 2), FieldPred::u64_eq("pure_frames", 3)],
            )).collect(),
        ),
''')
text += '''
#[test]
fn cli_pure_dispatch_scenario_passes() {
    let scenario = catalog().into_iter().find(|scenario| scenario.name == "determinism.cli_pure_segments.v1")
        .expect("native CLI pure dispatch scenario is registered");
    let report = Runner::from_env().run(scenario);
    assert!(report.is_pass(), "{}", report.summary());
}
'''
pending[path] = text
for source, destination in NEW.items():
    pending[destination] = (PAYLOAD / source).read_text()
# Complete all parent/context validation before mutating any source.
for name, content in pending.items():
    path = ROOT / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
print('Applied native CLI pure dispatch, diagnostics, failure latching and acceptance tests.')
