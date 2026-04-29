-- Migration: normalize cfg.instrument_refdata.instrument_type to canonical
-- proto-aligned UPPERCASE names and lock with a CHECK constraint.
--
-- Source of truth: zk.common.v1.InstrumentType (UNSPECIFIED, SPOT, PERP,
-- FUTURE, CFD, OPTION, ETF, STOCK). Idempotent: safe to re-run.

-- Step 1: drop the constraint if it already exists, so re-running with new
--         (additional) variants is non-destructive.
ALTER TABLE cfg.instrument_refdata
    DROP CONSTRAINT IF EXISTS instrument_refdata_type_chk;

-- Step 2: canonicalize existing rows. Most legacy IBKR rows are lowercase
--         ("stock", "etf"); crypto venue rows already UPPERCASE. UPPER() is a
--         no-op for the latter. Map legacy/non-canonical names to the proto-
--         aligned canonical name where possible.
UPDATE cfg.instrument_refdata
   SET instrument_type = CASE UPPER(COALESCE(instrument_type, ''))
       WHEN ''         THEN 'UNSPECIFIED'
       WHEN 'SPOT'     THEN 'SPOT'
       WHEN 'PERP'     THEN 'PERP'
       WHEN 'FUTURE'   THEN 'FUTURE'
       WHEN 'CFD'      THEN 'CFD'
       WHEN 'OPTION'   THEN 'OPTION'
       WHEN 'ETF'      THEN 'ETF'
       WHEN 'STOCK'    THEN 'STOCK'
       WHEN 'FOREX'    THEN 'SPOT'      -- legacy IBKR cash → spot
       WHEN 'EQUITY'   THEN 'STOCK'     -- legacy generic equity → stock
       WHEN 'UNSPECIFIED' THEN 'UNSPECIFIED'
       WHEN 'UNKNOWN'  THEN 'UNSPECIFIED'
       ELSE UPPER(instrument_type)      -- last-ditch: just uppercase; CHECK below catches the rest
   END
   WHERE instrument_type IS NULL
      OR instrument_type !~ '^(UNSPECIFIED|SPOT|PERP|FUTURE|CFD|OPTION|ETF|STOCK)$';

-- Step 3: lock future writes to the canonical set.
ALTER TABLE cfg.instrument_refdata
    ADD CONSTRAINT instrument_refdata_type_chk
    CHECK (instrument_type IN (
        'UNSPECIFIED', 'SPOT', 'PERP', 'FUTURE',
        'CFD', 'OPTION', 'ETF', 'STOCK'
    ));
