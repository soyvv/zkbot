"""Shared pytest fixtures for zk-refdata-svc tests."""

import os

import asyncpg
import pytest

PG_URL = os.getenv("ZK_TEST_PG_URL", "postgres://zk:zk@localhost:5432/zkbot")


@pytest.fixture(scope="session")
async def pg_pool():
    pool = await asyncpg.create_pool(PG_URL, min_size=1, max_size=3)
    yield pool
    await pool.close()


@pytest.fixture
async def conn(pg_pool):
    """Each test gets a connection wrapped in a rolled-back transaction."""
    async with pg_pool.acquire() as connection:
        tr = connection.transaction()
        await tr.start()
        yield connection
        await tr.rollback()
