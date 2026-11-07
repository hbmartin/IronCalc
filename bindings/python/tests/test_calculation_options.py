import ast
from pathlib import Path

import ironcalc as ic
import pytest


@pytest.mark.parametrize("factory,reload", [(ic.create, ic.load_from_bytes), (ic.UserModel, ic.UserModel.from_bytes)])
def test_two_pass_option_is_local_runtime_configuration(factory, reload):
    model = factory("comparison")
    assert model.get_require_two_iterative_passes() is True
    model.set_require_two_iterative_passes(False)
    assert model.get_require_two_iterative_passes() is False
    restored = reload(model.to_bytes(), "en")
    assert restored.get_require_two_iterative_passes() is True
    model.set_require_two_iterative_passes(True)
    assert model.get_require_two_iterative_passes() is True
    if isinstance(model, ic.UserModel):
        assert model.can_undo() is False


def test_type_stubs_expose_option_on_both_models():
    stub = Path(__file__).parents[1] / "python/ironcalc/_ironcalc.pyi"
    tree = ast.parse(stub.read_text())
    classes = {node.name: node for node in tree.body if isinstance(node, ast.ClassDef)}
    assert {"Model", "UserModel"} <= classes.keys()
    for name in ("Model", "UserModel"):
        methods = {method.name: method for method in classes[name].body if isinstance(method, ast.FunctionDef)}
        setter = methods["set_require_two_iterative_passes"]
        assert ast.unparse(setter.args.args[1].annotation) == "bool"
        getter = methods["get_require_two_iterative_passes"]
        assert ast.unparse(getter.returns) == "bool"


def test_loading_formats_saved_results_without_recalculating_rand():
    model = ic.UserModel("saved")
    model.set_user_input(0, 1, 1, "=RAND()")
    model.add_conditional_formatting(0, "B1", {
        "type": "Formula", "formula": "$A$1>=0",
        "format": {"font": {"b": True}}, "stop_if_true": False,
    })
    value = model.get_cell_value(0, 1, 1)
    saved = model.to_bytes()
    restored = ic.UserModel.from_bytes(saved, "en")
    assert restored.get_cell_value(0, 1, 1) == value
    assert restored.to_bytes() == saved
