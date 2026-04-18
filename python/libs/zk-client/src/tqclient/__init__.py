from importlib import import_module
import sys
import warnings

_TARGET = "zk_client"
warnings.warn(
    "tqclient package is deprecated; use zk_client instead.",
    DeprecationWarning,
    stacklevel=2,
)

module = import_module(_TARGET)
globals().update(module.__dict__)
__all__ = getattr(module, "__all__", [name for name in globals() if not name.startswith("_")])


def __getattr__(name: str):
    if name in globals():
        return globals()[name]
    target_name = f"{_TARGET}.{name}"
    try:
        submodule = import_module(target_name)
    except ModuleNotFoundError as exc:
        raise AttributeError(name) from exc
    sys.modules[f"{__name__}.{name}"] = submodule
    return submodule


def __dir__():
    return sorted(__all__)
