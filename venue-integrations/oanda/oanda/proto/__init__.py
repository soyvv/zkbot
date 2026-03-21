"""Proto stubs package — adds proto directory to sys.path for generated import resolution."""

import sys
from pathlib import Path

# Generated proto stubs use absolute imports like `from zk.common.v1 import common_pb2`.
# Add this directory to sys.path so those imports resolve.
_proto_dir = str(Path(__file__).parent)
if _proto_dir not in sys.path:
    sys.path.insert(0, _proto_dir)
