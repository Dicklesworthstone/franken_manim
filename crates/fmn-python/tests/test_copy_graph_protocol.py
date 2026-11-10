"""Production copy-graph orchestration over explicit native shell fixtures.

The real installed-extension counterpart exercises Marionette-bound and
nursery-owned mobjects, mutation independence, and Animation.copy aliases.
"""
import ast
import copy
import types
import unittest

from creation_protocol_support import BOOTSTRAP


def environment():
    g = {"_copy": copy, "_types": types, "_FamilyCycleError": ValueError,
         "_CameraFrameCore": type("CameraFrameCore", (), {})}
    wanted = {"_family_preorder", "_copy_mobject_graph", "_copy_member_state",
              "_COPY_RELATIONSHIP_ATTRIBUTES"}

    def lifted(node):
        if isinstance(node, ast.FunctionDef):
            return node.name in wanted
        return isinstance(node, ast.Assign) and any(
            getattr(target, "id", None) in wanted for target in node.targets)

    nodes = [node for node in ast.parse(BOOTSTRAP.read_text()).body if lifted(node)]
    exec(compile(ast.Module(nodes, []), str(BOOTSTRAP), "exec"), g)

    def install(node):
        node.submobjects = []
        node.uniforms = types.SimpleNamespace(_extras={})
        node.updaters, node.parents = [], []
        node.saved_state = node.target = None

    class Node:
        def __init__(self, name, *children, bound=False):
            install(self)
            self.name, self.bound = name, bound
            self.payload = [name]
            self.submobjects.extend(children)
        def __deepcopy__(self, memo):
            return g["_copy_mobject_graph"](self, True, memo)
        def _is_bound(self):
            return self.bound
        def _copy_family_shells(self):
            if not self.bound:
                return None
            return [(old, type(old).__new__(type(old)))
                    for old in g["_family_preorder"](self)]
        def _copy_detached_state_to(self, target):
            target.native_marker = "copied nursery"
        def _engine_state(self):
            return {"native_marker": "restored native state"}
        def _restore_engine_state(self, state):
            self.__dict__.update(state)

    g.update(_BridgeMobject=Node, _install_live_state=install)
    return Node, g["_copy_mobject_graph"]


def time_field_environment():
    """Real time-field/copy dispatch over a non-rendering native-shell fixture."""
    Node, clone = environment()
    namespace = {}

    class VectorField(Node):
        def __init__(self, function, coordinate_system, **kwargs):
            super().__init__("field")
            self.func = function
            self.coordinate_system = coordinate_system

        def add_updater(self, callback):
            self.updaters.append(callback)
            return self

        @property
        def always(self):
            return namespace["_UpdaterBuilder"](self)

        def update_vectors(self):
            self.observation = self.func([[0., 0.], [1., 0.]])
            return self

    namespace["VectorField"] = VectorField
    names = {"TimeVaryingVectorField", "_UpdaterBuilder"}
    nodes = [node for node in ast.parse(BOOTSTRAP.read_text()).body
             if isinstance(node, ast.ClassDef) and node.name in names]
    exec(compile(ast.Module(nodes, []), str(BOOTSTRAP), "exec"), namespace)
    return namespace["TimeVaryingVectorField"], clone


class CopyGraphTests(unittest.TestCase):
    def setUp(self):
        self.Node, self.clone = environment()

    def test_descendant_before_root_reuses_one_authoritative_copy(self):
        for bound in (False, True):
            leaf = self.Node("leaf", bound=bound)
            root = self.Node("root", leaf, bound=bound)
            root.alias = [leaf]
            copied_leaf, copied_root = copy.deepcopy([leaf, root])
            self.assertIs(copied_root.submobjects[0], copied_leaf)
            self.assertIs(copied_root.alias[0], copied_leaf)
            self.assertIsNot(copied_leaf, leaf)
            copied_leaf.payload.append("edited")
            self.assertEqual(leaf.payload, ["leaf"])

    def test_two_root_graphs_share_the_same_copied_descendant(self):
        for bound in (False, True):
            leaf = self.Node("leaf", bound=bound)
            left = self.Node("left", leaf, bound=bound)
            right = self.Node("right", leaf, bound=bound)
            a, b = copy.deepcopy([left, right])
            self.assertIs(a.submobjects[0], b.submobjects[0])
            self.assertIsNot(a.submobjects[0], leaf)

    def test_root_before_alias_keeps_existing_behavior(self):
        leaf = self.Node("leaf")
        root = self.Node("root", leaf)
        copied_root, copied_leaf = copy.deepcopy([root, leaf])
        self.assertIs(copied_root.submobjects[0], copied_leaf)

    def test_prepopulated_memo_is_not_overwritten_or_reinitialized(self):
        for bound in (False, True):
            leaf = self.Node("leaf", bound=bound)
            child = self.Node("child", leaf, bound=bound)
            root = self.Node("root", child, bound=bound)
            copied_child = copy.deepcopy(child)
            copied_child.payload.append("retained edit")
            sentinel = object()
            copied_child.uniforms._extras["sentinel"] = sentinel
            copied_child.updaters.append(sentinel)
            original_members = tuple(copied_child.submobjects)
            memo = {id(child): copied_child, id(leaf): original_members[0]}
            copied_root = copy.deepcopy(root, memo)
            self.assertIs(copied_root.submobjects[0], copied_child)
            self.assertIs(memo[id(child)], copied_child)
            self.assertEqual(tuple(copied_child.submobjects), original_members)
            self.assertEqual(copied_child.payload, ["child", "retained edit"])
            self.assertIs(copied_child.uniforms._extras["sentinel"], sentinel)
            self.assertEqual(copied_child.updaters, [sentinel])

    def test_animation_dictionary_aliases_before_root_remain_coherent(self):
        leaf = self.Node("leaf")
        root = self.Node("root", leaf)
        animation = types.SimpleNamespace(all_submobs=[leaf], mobject=root)
        duplicate = copy.deepcopy(animation)
        self.assertIs(duplicate.all_submobs[0], duplicate.mobject.submobjects[0])

    def test_detached_snapshot_branch_respects_shared_memo(self):
        leaf = self.Node("leaf", bound=True)
        root = self.Node("root", leaf, bound=True)
        copied_leaf = self.Node("already copied")
        duplicate = self.clone(root, True, {id(leaf): copied_leaf}, detach_bound=True)
        self.assertIs(duplicate.submobjects[0], copied_leaf)
        self.assertEqual(copied_leaf.name, "already copied")

    def test_attribute_cycles_are_remapped_to_copied_root(self):
        leaf = self.Node("leaf")
        root = self.Node("root", leaf)
        root.alias = leaf
        leaf.owner = root
        copied_leaf, copied_root = copy.deepcopy([leaf, root])
        self.assertIs(copied_leaf.owner, copied_root)
        self.assertIs(copied_root.alias, copied_leaf)
        self.assertIs(copied_root.submobjects[0], copied_leaf)

    def test_shallow_copy_keeps_unrelated_aliases_and_separates_family(self):
        leaf, external = self.Node("leaf"), self.Node("external")
        root = self.Node("root", leaf)
        root.alias, root.external, root.nested = leaf, external, [leaf]
        duplicate = self.clone(root, False, {})
        self.assertIs(duplicate.alias, duplicate.submobjects[0])
        self.assertIsNot(duplicate.alias, leaf)
        self.assertIs(duplicate.external, external)
        self.assertIs(duplicate.nested, root.nested)

    def test_bound_method_attributes_follow_the_copied_family(self):
        for bound in (False, True):
            leaf, external = self.Node("leaf", bound=bound), self.Node("external")
            root = self.Node("root", leaf, bound=bound)
            read_name = lambda receiver: receiver.name
            root.own = types.MethodType(read_name, root)
            root.child_method = types.MethodType(read_name, leaf)
            leaf.owner_method = root.own
            root.external_method = types.MethodType(read_name, external)
            callback = lambda obj: None
            root.callback = callback
            root.updaters.append(root.own)
            duplicate = self.clone(root, False, {})
            self.assertIs(duplicate.own.__self__, duplicate)
            self.assertIs(duplicate.child_method.__self__, duplicate.submobjects[0])
            self.assertIs(duplicate.submobjects[0].owner_method.__self__, duplicate)
            self.assertIs(duplicate.child_method.__func__, read_name)
            self.assertIs(duplicate.external_method, root.external_method)
            self.assertIs(duplicate.callback, callback)
            self.assertIs(duplicate.updaters[0], root.updaters[0])
            duplicate.submobjects[0].name = "copied leaf"
            self.assertEqual(duplicate.child_method(), "copied leaf")
            self.assertEqual(root.child_method(), "leaf")

    def test_time_varying_copies_sample_their_own_clock(self):
        Field, clone = time_field_environment()
        callback = lambda rows, time: (len(rows), time)
        for bound in (False, True):
            for deep in (False, True):
                with self.subTest(bound=bound, deep=deep):
                    field = Field(callback, object())
                    field.bound = bound
                    field.increment_time(.25)
                    duplicate = clone(field, deep, {})
                    # Exercise the real constructor's two registered updaters:
                    # advance the receiver's clock, then resample its field.
                    duplicate.updaters[0](duplicate, .5)
                    duplicate.updaters[1](duplicate)
                    self.assertEqual(duplicate.observation, (2, .75))
                    self.assertEqual(field.time, .25)
                    field.increment_time(1.)
                    duplicate.update_vectors()
                    self.assertEqual(duplicate.observation, (2, .75))
                    self.assertIs(duplicate.time_func, callback)
                    self.assertIs(duplicate.func.__self__, duplicate)


if __name__ == "__main__":
    unittest.main()
