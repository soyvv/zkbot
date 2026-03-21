"""Test fixtures for the OANDA venue adaptor."""

import sys
from pathlib import Path

# Add venue-integrations/oanda/ to path so `oanda` package resolves.
_oanda_root = Path(__file__).parent.parent
if str(_oanda_root) not in sys.path:
    sys.path.insert(0, str(_oanda_root))
