-- Baseline migration. The actual schema is managed by devops/init/postgres/*.sql
-- and is already present in the database. This file exists so Flyway can track
-- migration state. Use flyway.baseline-on-migrate=true for existing databases.
SELECT 1;
