"""Native text/TeX subclass hooks, persistent schemas and live source spans."""
from pathlib import Path
import tempfile
import inspect
import unittest

import numpy as np
import manimlib as m


class StringLifecycleTests(unittest.TestCase):
    def test_all_string_front_doors_dispatch_hooks_once(self):
        for base, text in ((m.Text,'abc'),(m.MarkupText,'<b>abc</b>'),(m.Code,'x = 1'),
                           (m.Tex,'x^2'),(m.TexText,'abc')):
            with self.subTest(base=base.__name__):
                class Custom(base):
                    def init_data(self):
                        self.events=['data'];self.source_at_init=self.string
                        super().init_data()
                    def init_points(self):self.events.append('points');super().init_points()
                    def init_uniforms(self):self.events.append('uniforms');super().init_uniforms()
                    def init_colors(self):self.events.append('colors');super().init_colors()
                obj=Custom(text)
                self.assertEqual(obj.events,['data','points','uniforms','colors'])
                self.assertEqual(obj.source_at_init,text)
                self.assertGreater(len(obj.family_members_with_points()),0)

    def test_custom_schema_decoration_and_span_paths_survive(self):
        for base, text in ((m.Text,'é + é'),(m.Tex,'x + x')):
            with self.subTest(base=base.__name__):
                class Custom(base):
                    data_dtype=m.VMobject.data_dtype+[('custom_lane',1)]
                    def init_data(self):
                        super().init_data();self.decoration=m.Dot().shift(m.DOWN)
                        self.add(self.decoration)
                    def init_points(self):
                        super().init_points()
                        self.set_points_as_corners([m.LEFT,m.RIGHT])
                        self.data['custom_lane'][:]=9
                    def init_uniforms(self):
                        super().init_uniforms();self.uniforms['is_fixed_in_frame']=1
                obj=Custom(text)
                self.assertIn(obj.decoration,obj.submobjects)
                np.testing.assert_array_equal(obj.data['custom_lane'],9)
                self.assertEqual(obj.uniforms['is_fixed_in_frame'],1)
                self.assertTrue(all(path[0]>0 for path in obj._string_sub_paths))
                matches=obj.select_parts('é' if base is m.Text else 'x')
                self.assertEqual(len(matches),2)
                self.assertNotIn(obj.decoration,matches.get_family())
                copied=obj.copy()
                self.assertIsNot(copied.decoration,obj.decoration)
                self.assertIn(copied.decoration,copied.submobjects)
                self.assertEqual(len(copied.select_parts('é' if base is m.Text else 'x')),2)

    def test_custom_init_points_controls_rendered_glyph_geometry(self):
        for base in (m.Text,m.Tex):
            class Raised(base):
                def init_points(self):
                    super().init_points();self.stretch(2,1,about_point=m.ORIGIN)
            plain=base('abc',should_center=False)
            custom=Raised('abc',should_center=False)
            self.assertAlmostEqual(custom.get_height(),2*plain.get_height(),places=6)
            self.assertAlmostEqual(custom.get_width(),plain.get_width(),places=6)

    def test_custom_init_colors_not_overwritten_by_default_style(self):
        for base in (m.Text,m.Tex):
            class Custom(base):
                def init_colors(self):super().init_colors();self.set_color(m.GREEN)
            obj=Custom('ab')
            self.assertTrue(all(part.get_fill_color()==m.GREEN for part in obj.family_members_with_points()))

    def test_native_markup_colors_and_code_highlighting_survive(self):
        text=m.MarkupText('<span foreground="#FC6255">A</span>B')
        self.assertEqual(text.select_part('A').family_members_with_points()[0].get_fill_color(),m.RED)
        self.assertNotEqual(text.select_part('B').family_members_with_points()[0].get_fill_color(),m.RED)
        code=m.Code('def f(x):\n    return 42')
        colors={tuple(part.data['fill_rgba'][0]) for part in code.family_members_with_points()}
        self.assertGreater(len(colors),1)

    def test_explicit_style_and_source_maps_win_over_native_defaults(self):
        text=m.Text('ab',fill_color=m.GREEN,t2c={'b':m.RED},stroke_width=2,stroke_color=m.BLUE)
        self.assertEqual(text.select_part('a').family_members_with_points()[0].get_fill_color(),m.GREEN)
        self.assertEqual(text.select_part('b').family_members_with_points()[0].get_fill_color(),m.RED)
        self.assertAlmostEqual(text.select_part('a').family_members_with_points()[0].get_stroke_width(),2)
        self.assertEqual(text.select_part('a').family_members_with_points()[0].get_stroke_color(),m.BLUE)
        tex=m.Tex('x+y',tex_to_color_map={'x':m.RED},fill_color=m.GREEN)
        self.assertEqual(tex.select_part('x').family_members_with_points()[0].get_fill_color(),m.RED)
        self.assertEqual(tex.select_part('y').family_members_with_points()[0].get_fill_color(),m.GREEN)

    def test_nested_tex_span_paths_skip_hook_decorations(self):
        class Custom(m.Tex):
            def init_data(self):super().init_data();self.dot=m.Dot();self.add(self.dot)
        tex=Custom('x','+','y',tex_to_color_map={'x':m.RED,'y':m.GREEN})
        self.assertIn(tex.dot,tex.submobjects)
        self.assertEqual(tex.select_part('x').family_members_with_points()[0].get_fill_color(),m.RED)
        self.assertEqual(tex.select_part('y').family_members_with_points()[0].get_fill_color(),m.GREEN)
        self.assertNotIn(tex.dot,tex.select_parts('x').get_family())
        self.assertTrue(all(len(path)>1 and path[0]>0 for path in tex._string_sub_paths))

    def test_reinitialization_replaces_only_native_glyph_children(self):
        class Custom(m.Text):
            def init_data(self):super().init_data();self.dot=m.Dot();self.add(self.dot)
        text=Custom('ab');old=tuple(text._fmn_string_children)
        decoration=m.Square();text.add(decoration)
        text.text='xyz';text.init_points()
        self.assertIn(text.dot,text.submobjects);self.assertIn(decoration,text.submobjects)
        self.assertFalse(any(child in text.submobjects for child in old))
        self.assertEqual(len(text.select_parts('x')),1)
        self.assertTrue(all(path[0]>=2 for path in text._string_sub_paths))
        scene=m.Scene();scene.add(text);text.init_points()
        self.assertIn(text,scene.mobjects)
        self.assertEqual(len(text.select_parts('x')),1)

    def test_tex_signature_and_code_regeneration_keep_public_contracts(self):
        signature=inspect.signature(m.Tex)
        self.assertEqual(signature.parameters['tex_strings'].annotation,'str')
        self.assertEqual(signature.parameters['font_size'].default,48)
        code=m.Code('return 1')
        code.init_points()
        self.assertGreater(len(code.family_members_with_points()),0)
        self.assertEqual(code.font,'Consolas')

    def test_custom_hook_failure_propagates_once(self):
        for hook in ('init_data','init_points','init_uniforms','init_colors'):
            calls=[];failure=RuntimeError(hook)
            def broken(self):calls.append(hook);raise failure
            cls=type('Broken',(m.Text,),{hook:broken})
            with self.subTest(hook=hook),self.assertRaises(RuntimeError) as caught:cls('ab')
            self.assertIs(caught.exception,failure);self.assertEqual(calls,[hook])

    def test_native_typesetter_failure_preserves_existing_family(self):
        text=m.Text('ab');old=list(text.submobjects);spans=list(text._string_sub_spans)
        text.font='not-a-bundled-font-at-all'
        with self.assertRaises(Exception):text.init_points()
        self.assertEqual(list(text.submobjects),old);self.assertEqual(text._string_sub_spans,spans)

    def test_full_override_can_supply_geometry_without_a_span_map(self):
        class Custom(m.Text):
            def init_points(self):self.add(m.Square(fill_opacity=1,stroke_width=0))
        obj=Custom('not typeset')
        self.assertEqual(obj._string_sub_paths,[])
        self.assertAlmostEqual(obj.get_width(),2)
        self.assertEqual(len(obj.select_parts('not')),0)

    def test_matching_transform_uses_real_custom_text(self):
        class Custom(m.Text):
            def init_points(self):super().init_points();self.stretch(2,1)
        source=Custom('x+y');target=Custom('y+x').shift(m.RIGHT)
        scene=m.Scene();scene.add(source)
        scene.play(m.TransformMatchingStrings(source,target),run_time=2/30)
        self.assertIn(target,scene.mobjects);self.assertNotIn(source,scene.mobjects)
        self.assertEqual(len(target.select_parts('x')),1)

    def test_native_glyph_frames_match_literal_posttransform_and_threads(self):
        def render(path,base,custom,threads):
            scene=m.Scene()
            with scene.render_session(path,format='png_sequence',resolution=(120,68),fps=4,threads=threads):
                if custom:
                    class Custom(base):
                        def init_points(self):super().init_points();self.stretch(1.5,1,about_point=m.ORIGIN)
                    text=Custom('x+y',should_center=False)
                else:
                    text=base('x+y',should_center=False).stretch(1.5,1,about_point=m.ORIGIN)
                scene.add(text);scene.play(text.animate.shift(m.RIGHT),run_time=.5,rate_func=m.linear)
            return [p.read_bytes() for p in sorted(path.glob('*.png'))]
        root=Path(tempfile.mkdtemp(prefix='fmn-string-lifecycle-'))
        for base in (m.Text,m.Tex):
            with self.subTest(base=base.__name__):
                first=render(root/(base.__name__+'one'),base,True,1)
                self.assertEqual(first,render(root/(base.__name__+'four'),base,True,4))
                self.assertEqual(first,render(root/(base.__name__+'expected'),base,False,1))
                self.assertEqual(len(first),2);self.assertNotEqual(first[0],first[1])


if __name__=='__main__':unittest.main()
