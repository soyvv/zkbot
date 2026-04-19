"""Dev-only strategy loading helpers.

Production strategy loading must use :func:`zk_strategy.strategy_core.load_strategy`
with an installed ``module:class`` entrypoint. The helpers in this module load
strategies directly from a file path by mutating ``sys.path``; they exist solely
for tests, notebooks, and the backtester driver where the strategy source lives
outside any installed distribution.

The contract (see ``docs/system-arch/dependency-contract.md``):

- ``sys.path`` mutation is forbidden outside this module.
- The dependency-contract grep audit explicitly whitelists ``zk_strategy/dev.py``.
"""

from __future__ import annotations

import importlib.util
import os
import sys
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator, Type

from .strategy_base import StrategyBase


@contextmanager
def _sys_path_prepend(path: str) -> Iterator[None]:
    """Prepend ``path`` to ``sys.path`` and remove the exact entry on exit."""
    if path in sys.path:
        yield
        return
    sys.path.insert(0, path)
    try:
        yield
    finally:
        try:
            sys.path.remove(path)
        except ValueError:
            pass


def load_from_file(file_path: str) -> Type[StrategyBase]:
    """Load a ``StrategyBase`` subclass from a Python source file.

    Dev/test-only. Appends the file's directory to ``sys.path`` for the duration
    of the import so sibling imports resolve, then removes that entry.
    """
    if not Path(file_path).exists():
        raise FileNotFoundError(f"strategy source file not found: {file_path}")

    module_name = os.path.splitext(os.path.basename(file_path))[0]
    module_dir = os.path.dirname(os.path.abspath(file_path))

    with _sys_path_prepend(module_dir):
        spec = importlib.util.spec_from_file_location(module_name, file_path)
        if spec is None or spec.loader is None:
            raise ImportError(f"could not build import spec for {file_path}")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)

    strategy_cls = None
    for _, obj in module.__dict__.items():
        if (
            isinstance(obj, type)
            and issubclass(obj, StrategyBase)
            and obj is not StrategyBase
            and getattr(obj, "__module__", None) == module.__name__
        ):
            strategy_cls = obj
            break

    if strategy_cls is None:
        raise ImportError(f"no StrategyBase subclass found in {file_path}")

    init_func = module.__dict__.get("__tq_init__")
    if callable(init_func):
        strategy_cls.__tq_init__ = init_func
    return strategy_cls
