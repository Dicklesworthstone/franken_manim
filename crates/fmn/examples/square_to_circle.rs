use fmn::prelude::*;

#[derive(Default)]
struct SquareToCircle;

impl SceneConstruct for SquareToCircle {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let tex = TexEngine::new("fmd-math/pack/default", None)?;
        let square = stage.add(Square::new().side_length(2.0).color(BLUE))?;
        let label = stage
            .add(Tex::new(r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}").build(&tex)?)?;
        stage.next_to(label, square, UP, DEFAULT_MOBJECT_TO_MOBJECT_BUFF, ORIGIN);

        let write_label = write(stage.arena(), label);
        stage.play((show_creation(square), write_label))?;
        let restyle = square.animate().rotate(PI / 4.0)?.set_opacity(0.5)?;
        stage.play(restyle)?;

        let circle = stage
            .arena_mut()
            .add(Circle::new().radius(1.0).color(YELLOW));
        stage.play(Transform::new(square, circle))?;
        stage.wait(1.0)?;
        Ok(())
    }
}

fn main() -> fmn::Result<()> {
    let mut sink = NullSceneSink;
    let _completed = run_scene(&mut SquareToCircle, RuntimeConfig::default(), 0, &mut sink)?;
    Ok(())
}
