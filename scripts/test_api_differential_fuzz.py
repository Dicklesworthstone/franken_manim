"""Unit tests for the differential API fuzzer (fm-5wq.34). No engine runs here."""
import ast
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import api_differential_fuzz as fuzz  # noqa: E402

SCHEMA = fuzz.ROOT / "API_SCHEMA.tsv"


def read_schema(path=SCHEMA):
    """Exported classes with their bases, every `Class.method`, and each owner's parameters."""
    classes, methods, params = {}, set(), {}
    section = None
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            section = line.strip("[]")
            continue
        row = line.split("\t")
        if section == "symbols":
            module, name, kind, origin, exported, detail = row
            if kind == "class" and exported == "1":
                classes[name] = [base.strip() for base in detail.split(",") if base.strip()]
            elif kind == "method":
                methods.add(name)
        elif section == "params":
            owner, _, name, kind = row[:4]
            params.setdefault(owner.split(":", 1)[1], []).append((name, kind))
    return classes, methods, params


def accepted_keywords(owner, classes, params, method):
    """Keyword names `owner.method` accepts, following the bases while **kwargs forwards."""
    names, seen, todo = set(), set(), [owner]
    while todo:
        cls = todo.pop()
        if cls in seen:
            continue
        seen.add(cls)
        rows = params.get(f"{cls}.{method}")
        if rows is None:
            todo.extend(classes.get(cls, ()))
            continue
        names.update(name for name, kind in rows if kind != "var_keyword")
        if any(kind == "var_keyword" for _, kind in rows):
            todo.extend(classes.get(cls, ()))
    return names


class GeneratorTests(unittest.TestCase):
    def test_same_seed_and_index_give_the_same_bytes(self):
        for index in range(50):
            self.assertEqual(fuzz.generate_program(7, index), fuzz.generate_program(7, index))
        self.assertNotEqual(fuzz.generate_program(7, 0)["source"], fuzz.generate_program(8, 0)["source"])

    def test_programs_parse_and_name_only_the_reference_api(self):
        classes, methods, params = read_schema()
        method_names = {name.split(".", 1)[1] for name in methods}
        constructed = set()
        for index in range(300):
            program = fuzz.generate_program(11, index)
            tree = ast.parse(program["source"])
            for node in ast.walk(tree):
                if not isinstance(node, ast.Call):
                    continue
                if isinstance(node.func, ast.Name):
                    name = node.func.id
                    self.assertIn(name, classes, f"{name} is not an exported Reference class")
                    constructed.add(name)
                    accepted = accepted_keywords(name, classes, params, "__init__")
                    for keyword in node.keywords:
                        self.assertIn(keyword.arg, accepted, f"{name}({keyword.arg}=...)")
                elif isinstance(node.func, ast.Attribute) and not (
                        isinstance(node.func.value, ast.Name) and node.func.value.id == "np"):
                    self.assertIn(node.func.attr, method_names, f".{node.func.attr}() is not in the schema")
        # The grammar reaches most of its constructors in 300 programs.
        self.assertGreaterEqual(len(constructed & set(fuzz.BY_CLASS)), len(fuzz.CONSTRUCTORS) - 2)

    def test_omitted_constructs_never_appear(self):
        for index in range(300):
            source = fuzz.generate_program(13, index)["source"]
            for name in fuzz.OMITTED:
                self.assertNotIn(f"{name}(", source)

    def test_view_writes_come_after_every_other_statement(self):
        # C-35: the Reference's box ignores a write through a view, so nothing may read
        # a box after one.
        for index in range(300):
            statements = fuzz.generate_program(17, index)["statements"]
            writes = [i for i, line in enumerate(statements)
                      if line.startswith("p") and (" += " in line or " *= " in line)]
            if writes:
                self.assertEqual(writes, list(range(writes[0], len(statements))))


class DdminTests(unittest.TestCase):
    def test_result_reproduces_and_is_minimal_for_a_conjunction(self):
        calls = []

        def reproduces(candidates):
            calls.append(len(candidates))
            return [{3, 7} <= set(c) for c in candidates]

        self.assertEqual(fuzz.ddmin(range(20), reproduces), [3, 7])

    def test_every_step_keeps_reproducing(self):
        kept = []

        def reproduces(candidates):
            verdicts = [sum(c) >= 30 and 5 in c for c in candidates]
            kept.extend(c for c, ok in zip(candidates, verdicts) if ok)
            return verdicts

        result = fuzz.ddmin(range(12), reproduces)
        self.assertTrue(sum(result) >= 30 and 5 in result)
        self.assertTrue(all(sum(c) >= 30 and 5 in c for c in kept))

    def test_budget_bounds_the_evaluations(self):
        spent = []

        def reproduces(candidates):
            spent.append(len(candidates))
            return [False] * len(candidates)

        result = fuzz.ddmin(range(64), reproduces, budget=40)
        self.assertEqual(result, list(range(64)))
        # The round that crosses the budget finishes; no further round starts.
        self.assertLess(sum(spent) - spent[-1], 40)


class ReplayTests(unittest.TestCase):
    def test_a_position_dependent_engine_is_counted(self):
        # run_batch is replaced by a fake engine whose facts depend on what ran before.
        calls = []

        def fake(engines, batch, work, engine):
            calls.append([p["id"] for p in batch])
            drift = engine == "portal"
            return [{"subject": p["id"], "members": [], "scene": [i if drift else 0]} for i, p in enumerate(batch)]

        engines = fuzz.Engines("portal", "reference", "root")
        original = fuzz.run_batch
        fuzz.run_batch = fake
        try:
            import tempfile
            with tempfile.TemporaryDirectory() as work:
                programs = [fuzz.generate_program(1, i) for i in range(3)]
                refs, portals = fuzz.run_both(engines, programs, fuzz.pathlib.Path(work))
        finally:
            fuzz.run_batch = original
        self.assertEqual(calls[0][-1], programs[0]["id"] + fuzz.REPLAY)
        self.assertEqual((len(refs), len(portals)), (3, 3))
        self.assertEqual(engines.position_mismatches, [programs[0]["id"]])


def _member(path, cls, bbox, **extra):
    return {"path": path, "class": cls, "mro": [cls], "bbox": bbox, **extra}


class CompareTests(unittest.TestCase):
    box = [[0, 0, 0], [500, 500, 0], [1000, 1000, 0]]
    moved = [[1000, 0, 0], [1500, 500, 0], [2000, 1000, 0]]

    def test_a_stale_reference_box_is_c35_when_the_portal_box_is_the_point_box(self):
        ref = {"subject": "p", "members": [_member("0", "Square", self.box, bbox_points=self.moved)]}
        portal = {"subject": "p", "members": [_member("0", "Square", self.moved)]}
        result = fuzz.compare(ref, portal, [])
        self.assertEqual(result["verdict"], "equal-with-exclusions")
        self.assertEqual(result["excluded"], {fuzz.C35: 1})

    def test_c35_also_corrects_the_box_derived_radius(self):
        # Circle.get_radius is |start - get_center()|, and get_center reads the stale box.
        points = [[1080, 500, 0], [1080, 520, 0], [1070, 540, 0]]
        fresh = [[920, 420, 0], [1000, 500, 0], [1080, 580, 0]]
        ref = {"subject": "p", "members": [_member("0", "Dot", self.box, bbox_points=fresh, points=points,
                                                   getters={"get_radius": 2518})]}
        portal = {"subject": "p", "members": [_member("0", "Dot", fresh, points=points,
                                                      getters={"get_radius": 80})]}
        self.assertEqual(fuzz.compare(ref, portal, [])["verdict"], "equal-with-exclusions")

    def test_a_stale_portal_box_is_a_difference(self):
        ref = {"subject": "p", "members": [_member("0", "Square", self.moved)]}
        portal = {"subject": "p", "members": [_member("0", "Square", self.box, bbox_points=self.moved)]}
        result = fuzz.compare(ref, portal, [])
        self.assertEqual(result["verdict"], "differs")
        self.assertIn(result["first_difference"]["fact"], ("bbox", "bbox_points"))

    def test_a_moved_object_stays_a_difference(self):
        ref = {"subject": "p", "members": [_member("0", "Square", self.box)]}
        portal = {"subject": "p", "members": [_member("0", "Square", self.moved)]}
        self.assertEqual(fuzz.compare(ref, portal, [])["first_difference"]["fact"], "bbox")

    def test_scene_order_and_absent_roots_are_compared(self):
        ref = {"subject": "p", "members": [], "scene": [0, 1], "absent_roots": []}
        portal = {"subject": "p", "members": [], "scene": [1, 0], "absent_roots": []}
        result = fuzz.compare(ref, portal, [])
        self.assertEqual(result["first_difference"]["fact"], "scene")

    def test_a_program_an_engine_never_ran_is_not_equal(self):
        exited = {"subject": "p", "error": "EngineExit: reference exited 1 before this program"}
        both = fuzz.compare(exited, {**exited, "error": "EngineExit: portal exited 1"}, [])
        self.assertEqual(both["verdict"], "engine-exit")
        one = fuzz.compare({"subject": "p", "members": []}, {**exited, "error": "EngineExit: portal"}, [])
        self.assertEqual((one["verdict"], one["exited"]), ("engine-exit", ["portal"]))

    def test_signature_keeps_the_error_classes(self):
        result = fuzz.compare({"subject": "p", "error": "ValueError: x"}, {"subject": "p", "members": []}, [])
        self.assertEqual(fuzz.signature(result), ("error", "ValueError", "none"))


if __name__ == "__main__":
    unittest.main()
