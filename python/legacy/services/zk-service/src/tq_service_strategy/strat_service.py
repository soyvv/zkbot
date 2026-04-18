import asyncio
import traceback
import inspect

import betterproto
import requests
import json
import traceback as tb
import uuid
import signal
import random
import time
import sys

from tq_service_strategy.engine import StrategyRealtimeEngine
from zk_client.tqclient import TQClient, TQClientConfig, TQOrder, TQCancel
from contextlib import suppress

from zk_strategy.api import *
from zk_strategy.strategy_core import StrategyTemplate, StrategyConfig

import zk_proto_betterproto.strategy as strat_pb
from tqrpc_utils.config_utils import try_load_config

from loguru import logger




@dataclass
class StrategyAppConfig:
    strategy_key: str = None
    ext_scripts_dir: str = None
    nats_url: str = None
    api_url: str = None
    instance_id: int = None
    skip_refdata_loading: bool = True



def _get_file_path(base_dir: str, file_name: str) -> str:
    import os
    return os.path.join(base_dir, file_name)

def initialize(engine: StrategyRealtimeEngine, conf: StrategyAppConfig):
    nats_url = conf.nats_url
    api_url = conf.api_url

    import sys
    sys.path.append(conf.ext_scripts_dir)

    execution_id = str(uuid.uuid4())
    strategy_execution = {
        "strategy_key": conf.strategy_key,
        "execution_id": execution_id
    }

    resp = requests.post(f"{api_url}/v1/tqstrat/strategy-inst", json=strategy_execution)
    if resp.status_code != 200:
        logger.error(f"Error fetching strategy data: {resp.text}")
        raise RuntimeError(f"Error fetching strategy data: {resp.text}")

    data = json.loads(resp.text)
    if data['success'] == False:
        logger.error(f"Strategy data contain error: {data}")
        raise RuntimeError(f"Error fetching strategy data: {resp.text}")

    if conf.ext_scripts_dir not in sys.path:
        sys.path.append(conf.ext_scripts_dir)

    execution = data['data']
    strategy_config = StrategyConfig()
    strategy_config.strategy_name = execution['strategy_key']
    strategy_config.strategy_file = _get_file_path(conf.ext_scripts_dir, execution['strategy_script_file_name'])
    strategy_config.custom_params = execution['config']
    strategy_config.symbols = execution['symbols']
    strategy_config.account_ids = execution['account_ids']
    tq_config_dict = execution['tq_config']
    tq_config_dict["nats_url"] = nats_url  # use the infra config by given env.
    tq_config = TQClientConfig(**tq_config_dict)

    logger.info(f"loaded strategy config = {strategy_config}")
    logger.info(f"loaded tq_config = {tq_config}")

    tq_client = TQClient(tq_config,
                         id_gen_instance=conf.instance_id,
                         source_id=strategy_config.strategy_name,
                         skip_refdata_loading=conf.skip_refdata_loading)

    engine._configure(
        strategy_config=strategy_config,
        tq_client=tq_client,
        engine_key=strategy_config.strategy_name,
        execution_id=execution_id,
        enable_startup_data_init=True,
        publish_strategy_event=True
    )

    return True


def run_strategy():
    engine_conf: StrategyAppConfig = try_load_config(StrategyAppConfig)
    logger.info(f"engine_conf={engine_conf}")

    engine = StrategyRealtimeEngine(instance_id=engine_conf.instance_id)

    try:
        successfully_init = initialize(engine, engine_conf)
        if not successfully_init:
            raise RuntimeError("Failed to initialize strategy")
        engine.run()
    except Exception as ex:
        msg = traceback.format_exc()
        logger.error(msg)

        # send lifecycle event(exception) if error
        if not isinstance(ex, KeyboardInterrupt):
            asyncio.run(engine._publish_lifecycle_event(
                strat_pb.StrategyStatusType.STRAT_STATUS_EXCEPTIONED,
                msg,
            ))

    finally:
        try:
            logger.info("sending lifecycle event: stopped")
            asyncio.run(engine._publish_lifecycle_event(
                strat_pb.StrategyStatusType.STRAT_STATUS_STOPPED))
        except:
            logger.error(traceback.format_exc())

        resp = requests.delete(
            f"{engine_conf.api_url}/v1/tqstrat/strategy-inst?strategy_key={engine_conf.strategy_key}"
        )
        logger.info(resp.text)


if __name__ == "__main__":
    run_strategy()
