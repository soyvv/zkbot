-- V11: Normalize the REFDATA topology model.
--
-- Invariant: one cfg.logical_instance row per env with instance_type='REFDATA'
-- (canonical name: refdata_<env>_1). Venue rows live ONLY in
-- cfg.refdata_venue_instance.
--
-- Prior to this migration, TopologyService.createRefdataVenueInstance inserted a
-- per-venue logical_instance row alongside each refdata_venue_instance row. This
-- polluted the topology namespace and broke the pilot-mode bootstrap flow.
--
-- This migration:
--   1. Cleans up dependent rows (instance_token, active_session, instance_id_lease)
--      that reference rogue per-venue REFDATA logical_ids.
--   2. Deletes the rogue REFDATA logical_instance rows (kept in refdata_venue_instance).
--   3. Seeds one refdata_<env>_1 row per env that has at least one
--      refdata_venue_instance row but no canonical REFDATA logical_instance row yet.

-- 1. Delete tokens/sessions/leases referencing rogue REFDATA logical_ids.
DELETE FROM cfg.instance_token
WHERE logical_id IN (
    SELECT li.logical_id
    FROM cfg.logical_instance li
    JOIN cfg.refdata_venue_instance rvi ON rvi.logical_id = li.logical_id
    WHERE li.instance_type = 'REFDATA'
);

DELETE FROM mon.active_session
WHERE logical_id IN (
    SELECT li.logical_id
    FROM cfg.logical_instance li
    JOIN cfg.refdata_venue_instance rvi ON rvi.logical_id = li.logical_id
    WHERE li.instance_type = 'REFDATA'
);

DELETE FROM cfg.instance_id_lease
WHERE logical_id IN (
    SELECT li.logical_id
    FROM cfg.logical_instance li
    JOIN cfg.refdata_venue_instance rvi ON rvi.logical_id = li.logical_id
    WHERE li.instance_type = 'REFDATA'
);

-- 2. Delete the rogue REFDATA logical_instance rows (per-venue pollution).
--    A REFDATA logical_instance row is rogue iff its logical_id also exists in
--    cfg.refdata_venue_instance; the canonical refdata_<env>_1 rows have no such match.
DELETE FROM cfg.logical_instance li
WHERE li.instance_type = 'REFDATA'
  AND EXISTS (
      SELECT 1 FROM cfg.refdata_venue_instance rvi
      WHERE rvi.logical_id = li.logical_id
  );

-- 3. Seed one canonical refdata_<env>_1 per env that owns at least one venue row
--    but has no REFDATA logical_instance yet. Idempotent via ON CONFLICT.
INSERT INTO cfg.logical_instance (logical_id, instance_type, env, enabled)
SELECT DISTINCT 'refdata_' || rvi.env || '_1', 'REFDATA', rvi.env, true
FROM cfg.refdata_venue_instance rvi
WHERE NOT EXISTS (
    SELECT 1 FROM cfg.logical_instance li
    WHERE li.instance_type = 'REFDATA' AND li.env = rvi.env
)
ON CONFLICT (logical_id) DO NOTHING;
