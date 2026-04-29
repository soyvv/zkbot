"""Dispatch a ``universe_source`` dict to the matching resolver."""

from __future__ import annotations

from ibkr.resolvers.base import UniverseResolver
from ibkr.resolvers.explicit import ExplicitResolver
from ibkr.resolvers.nasdaq_symbol_directory import NasdaqSymbolDirectoryResolver

_REGISTRY: dict[str, type] = {
    "explicit": ExplicitResolver,
    "nasdaq_symbol_directory": NasdaqSymbolDirectoryResolver,
}


def build_resolver(source: dict) -> UniverseResolver:
    """Build a resolver from a ``{type, config}`` discriminator dict."""
    if not isinstance(source, dict):
        raise ValueError(f"universe_source must be a dict, got {type(source).__name__}")
    src_type = source.get("type")
    if not src_type:
        raise ValueError("universe_source.type is required")
    cls = _REGISTRY.get(src_type)
    if cls is None:
        raise ValueError(
            f"unknown universe_source.type {src_type!r}; "
            f"known: {sorted(_REGISTRY)}"
        )
    return cls(source.get("config") or {})
