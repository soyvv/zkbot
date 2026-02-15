import traceback
import typing

from dataclasses import dataclass
from typing import Callable, Optional
import os, sys

import hvac
import argparse
import pprint

import logging

import json

logger = logging.getLogger(__name__)

@dataclass
class AccountSecrets:
    account_id: int = None
    exch_name: str = None
    exch_account_id: str = None
    auth_key: str = None
    auth_secret: str = None
    extra_secrets: dict[str, str] = None

class TQSecretLoader(typing.Protocol):
    def load_account_secret(self, account_id) -> AccountSecrets:
        pass

class TQSecretLoaderFromVault(TQSecretLoader):
    def __init__(self, url="http://localhost:5200", token=None, root_path="trading/gw", path=""):
        token = os.environ["VAULT_TOKEN"] if token is None or token == "" else token
        vault_url = os.environ["VAULT_URL"] if "VAULT_URL" in os.environ else url
        mount_path = os.environ["VAULT_MOUNT_PATH"] if "VAULT_MOUNT_PATH" in os.environ else root_path
        path_base = os.environ["VAULT_PATH_BASE"] if "VAULT_PATH_BASE" in os.environ else path

        self.vault_client = hvac.Client(url=vault_url)
        self.vault_client.token = token
        self.mount_point = mount_path
        self.path_base = path_base.rstrip("/") if path_base else ""

        assert self.vault_client.is_authenticated()

    def load_account_secret(self, account_id) -> AccountSecrets:
        try:
            path = f"{self.path_base}/{account_id}" if self.path_base else account_id
            secrets = self.vault_client.kv.read_secret(mount_point=self.mount_point, path=path)
        except Exception as ex:
            # todo: use log
            traceback.print_exc()
            return None
        data = secrets['data']['data']

        account_id = data['account_id']
        exch_name = data['exch_name']
        exch_account_id = data['exch_account_id']
        auth_key = data['auth_key']
        auth_secret = data['auth_secret']
        extra_secrets = data['extra_secrets']
        account_secrets = AccountSecrets(account_id=account_id, exch_name=exch_name, exch_account_id=exch_account_id,
                                         auth_key=auth_key, auth_secret=auth_secret,
                                         extra_secrets=extra_secrets)

        return account_secrets


class TQSecretLoaderFromEnv(TQSecretLoader):
    ENV_VAR_PREFIX = "GW_ACCOUNT_"
    ENV_VAR_EXTRA_PADDING = "_extra_secrets_"
    def load_account_secret(self, tq_account_id:int) -> AccountSecrets:
        extra_secrets_keys = [k for k in os.environ
                              if self.ENV_VAR_EXTRA_PADDING in k and k.startswith(self.ENV_VAR_PREFIX) ]

        if tq_account_id != int(os.environ.get(self._get_key(tq_account_id, "account_id"))):
            print("warning: account_id not match! {} != {}".format(tq_account_id, os.environ.get(self._get_key(tq_account_id, "account_id"))))
        account_id = tq_account_id
        exch_account_id = os.environ.get(self._get_key(account_id, "exch_account_id"))
        exch_name = os.environ.get(self._get_key(account_id, "exch_name"))
        auth_key = os.environ.get(self._get_key(account_id, "auth_key"))
        auth_secret = os.environ.get(self._get_key(account_id, "auth_secret"))
        extra_secrets = {k.replace(self.ENV_VAR_PREFIX, "").replace(self.ENV_VAR_EXTRA_PADDING, "").replace(str(account_id), ""): v
                        for k, v in os.environ.items() if k in extra_secrets_keys}

        account_secrets = AccountSecrets(account_id=account_id, exch_account_id=exch_account_id,
                                         exch_name=exch_name,
                                         auth_key=auth_key, auth_secret=auth_secret,
                                         extra_secrets=extra_secrets)

        return account_secrets


    def _get_key(self, account_id, key):
        key = f"{self.ENV_VAR_PREFIX}{account_id}_{key}"
        if key not in os.environ:
            raise KeyError(f"key {key} not found in env vars!")
        return key


class TQSecretLoaderFromFile(TQSecretLoader):
    def __init__(self, file:str):
        self.secret_file = file
        if not os.path.isfile(self.secret_file):
            print("error: secret file does not exist! {}".format(self.secret_file))
            raise KeyError(f"File {self.secret_file} does not exist!")

    def load_account_secret(self, tq_account_id:int) -> AccountSecrets:
        json_file = open(self.secret_file, "r")
        json_data = json.load(json_file)

        # The json when created by vault injector has the following form.
        # {
        #    "data":{
        #       "account_id":7696,
        #       "auth_key":"xxxxx",
        #       "auth_secret":"xxxx",
        #       "exch_account_id":"xxxxx",
        #       "exch_name":"xxxx",
        #       "extra_secrets":{}
        #    },
        #    "metadata":{
        #       "created_time":"2023-08-04T13:47:18.774705663Z",
        #       "custom_metadata":null,
        #       "deletion_time":"",
        #       "destroyed":false,
        #       "version":1
        #    }
        # }
        if "data" in json_data:
            account_secrets = AccountSecrets(**json_data["data"])
        else:
            account_secrets = AccountSecrets(**json_data)

        if tq_account_id != account_secrets.account_id:
            print("warning: account_id not match! {} != {}".format(tq_account_id, account_secrets.account_id))

        return account_secrets


def build_argparsor(cls_def: type) -> Callable[[Optional[str]], any]:
    # todo: validate the cls_def is a dataclass and all field-types are supported
    # todo: support int/float/str/bool/list[str]/enum

    fields = cls_def.__dataclass_fields__
    doc = cls_def.__doc__

    parser = argparse.ArgumentParser()
    for field in fields.values():
        f_type = field.type
        f_name = field.name
        # f_default = field.default
        # required = f_default is None or f_default == dataclasses._MISSING_TYPE
        if f_type == list[str]:
            parser.add_argument(f"--{f_name}", dest=f_name, nargs="+",
                                required=True)
        else:
            if f_type is bool:
                type_info = lambda x: (str(x).lower() in ['true', 'yes'])
            else:
                type_info = f_type

            parser.add_argument(f"--{f_name}",
                            type=type_info, dest=f_name,
                            required=True)

    def _parse_args_and_gen_config(cmd_args=None, print_when_constructed=True):
        args = None
        if cmd_args is None:
            args = parser.parse_args()
        else:
            args = parser.parse_args(cmd_args)

        if args:
            fields = cls_def.__dataclass_fields__
            for field in fields.values():
                f_name = field.name
                if f_name not in args.__dict__ or args.__dict__[f_name] is None:
                    parser.print_help()
                    sys.exit(-1)
            config = cls_def(**args.__dict__)
            if print_when_constructed:
                print(pprint.pformat(config, indent=2))
            return config
        else:
            return None

    return _parse_args_and_gen_config


def _is_list_str_type(f_type: type) -> bool:
    return getattr(f_type, "__origin__", None) is list and getattr(f_type, "__args__", None) == (str,)

def build_envvar_configgenerator(cls_def: type) -> Callable[[], any]:
    import os

    def _gen_config_from_envvars():
        fields = cls_def.__dataclass_fields__
        values = {}
        for field in fields.values():
            f_type = field.type
            f_name:str = field.name
            env_var_name = f_name.upper()
            env_var_val = os.environ.get(env_var_name)
            if env_var_val is None:
                raise Exception(f"Env var {env_var_name} is not set")

            if _is_list_str_type(f_type):
                f_value = env_var_val.split(",")
            elif f_type is bool:
                f_value = env_var_val.lower() in ('true', 'yes')
            else:
                f_value = f_type(env_var_val)

            values[f_name] = f_value

        config = cls_def(**values)
        return config

    return _gen_config_from_envvars


def merge_config_from_envvars(config: dict):
    def _traverse_dict(_dict: dict, _prefix: str):
        for k, v in _dict.items():
            if isinstance(v, dict):
                _traverse_dict(v, _prefix + k + "_")
            else:
                env_var_name = (_prefix + k).upper()
                env_var_val = os.environ.get(env_var_name)
                if env_var_val is not None:
                    _dict[k] = type(v)(env_var_val)
                else:
                    raise ValueError(f"Env var {env_var_name} is not set; need this value to construct config")

    _traverse_dict(config, "ev_")
    return config

#
# def _try_load_config_from_envvars() -> EnrichedEngineConfig:
#     from tqrpc_utils.rpc_utils import build_envvar_configgenerator
#     config_gen = build_envvar_configgenerator(EnrichedEngineConfig)
#     backoff_in_seconds = 1
#     retries = 0
#     while True:
#         try:
#             config: EnrichedEngineConfig = config_gen()
#             if config:
#                 return config
#         except Exception as ex:
#             msg = traceback.format_exc()
#             logger.error(msg)
#
#         # Let's not hammer the downstream dependencies.
#         sleep = (backoff_in_seconds * 2 ** retries + random.uniform(0, 1))
#         retries += 1
#         logger.info(f"Retried {retries} times, sleeping for {sleep} seconds.")
#         time.sleep(sleep)


def try_load_config(config_cls_def: type) -> any:
    import sys
    app_conf: config_cls_def = None
    if len(sys.argv) > 1:
        # load config info from args and infra_config
        print("args from commandline detected, loading config from args")

        parse_arg_func = build_argparsor(config_cls_def)
        app_conf: config_cls_def = parse_arg_func()

    else:
        # load config info from envvars; retry until success
        print("no args from commandline detected, loading config from envvars")
        config_gen = build_envvar_configgenerator(config_cls_def)
        app_conf: config_cls_def = config_gen()

    return app_conf


def try_load_secret(account_id: int) -> AccountSecrets:
    import sys, os
    secrets: AccountSecrets = None
    if "VAULT_TOKEN" in os.environ:
        print("VAULT_TOKEN found, loading secrets from vault")
        secrets = TQSecretLoaderFromVault().load_account_secret(account_id)
    elif "GW_SECRET_FILE" in os.environ:
        print("GW_SECRET_FILE found, loading secrets from json files")
        gw_secret_json = os.environ["GW_SECRET_FILE"]
        secret_loader = TQSecretLoaderFromFile(gw_secret_json)
        secrets = secret_loader.load_account_secret(account_id)
    else:
        raise Exception("No secret source found!")

    return secrets



def test_secret_loader():
    os.environ["GW_ACCOUNT_7696_account_id"] = "7696"
    os.environ["GW_ACCOUNT_7696_exch_account_id"] = "var2fdasfdsaf"
    os.environ["GW_ACCOUNT_7696_exch_name"] = "ORDERLY"
    # os.environ["GW_ACCOUNT_7696_auth_key"] = "authkey"
    os.environ["GW_ACCOUNT_7696_auth_secret"] = "authsecret"
    os.environ["GW_ACCOUNT_7696_extra_secrets_trading_key"] = "tradingkey"
    os.environ["GW_ACCOUNT_7696_extra_secrets_trading_secret"] = "tradingsecret"

    s = TQSecretLoaderFromEnv().load_account_secret(7696)

    print(s)


def test_config_loader():
    @dataclass
    class testconfig:
        var1: str = None
        var2: str = None
        var3: list[str] = None

    os.environ["VAR1"] = "var1"
    os.environ["VAR2"] = "var2"
    os.environ["VAR3"] = "var3,var4"

    args = try_load_config(testconfig)

    print(args)

if __name__ == '__main__':
    test_secret_loader()
