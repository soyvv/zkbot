# The purpose of this module is to run periodically and
# poll for new versions of the `tq-strategy-scripts` package.
import logging
import time
import sys
import subprocess
import os

from infra_config import tq_infra_config_loader

logging.basicConfig(format="%(levelname)s:%(filename)s:%(lineno)d:%(asctime)s:%(message)s", level=logging.DEBUG)
logger = logging.getLogger(__name__)

name = tq_infra_config_loader.EXT_SCRIPTS_PACKAGE_NAME
version = tq_infra_config_loader.EXT_SCRIPTS_VERSION
location = tq_infra_config_loader.EXT_SCRIPTS_DIR

# Tasks to perform:
#  * Check current version of package
#  * In not the latest version, update it and restart strategy
#
def updater():
    if install(name, version, location) or upgrade(name, version, location):
        restart_strategy()


def install(name, version, location: str) -> bool:
    if len(os.listdir(location)) > 0:
        return False

    logger.info(f"First time installation.")

    # We install in the main location.
    str(subprocess.run([
        sys.executable, '-m', 'pip', 'install',
        '{}{}'.format(name, version),
    ], capture_output=True, text=True))

    # We install in the specific directory.
    logs = str(subprocess.run([
        sys.executable, '-m', 'pip', 'install', '--upgrade',
        '--target', location,
        '{}{}'.format(name, version),
    ], capture_output=True, text=True))

    logger.info(f"pip first time installation logs {logs}.")

    return True


def upgrade(name, version, location: str) -> bool:
    # We install it in the main path to see if there are updates.
    logs = str(subprocess.run([
        sys.executable, '-m', 'pip', 'install', '--upgrade',
        '{}{}'.format(name, version),
    ], capture_output=True, text=True))
    logger.debug(f"pip install logs in the main directory {logs}.")

    if logs.find('Successfully installed ') != -1:
        logger.info(f"We have an updated version, forcing replacement.")
        logs = str(subprocess.run([
            sys.executable, '-m', 'pip', 'install', '--upgrade',
            '--target', location,
            '{}{}'.format(name, version),
        ], capture_output=True, text=True))

        logger.info(f"pip force install logs {logs}.")

        return True

    return False


def restart_strategy():
    if not tq_infra_config_loader.EXT_SCRIPTS_RESTART_SERVICE:
        logger.info("Not restarting strategy service.")
        return

    logger.info("Restarting strategy service.")

    # E.g.
    # root@tq-service-strategy-main-prod-5797dbb5b7-g5l56:/opt/app# ps uxf
    # USER         PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
    # root         240  0.0  0.0   2576   916 pts/2    Ss+  15:20   0:00 /bin/sh -c TERM=xterm-256color; export TERM; [ -x /bin/bash ] && ([ -x /usr/bin/script ] && /usr/bin/script -q -c "/bin/bash" /dev/null || exec /bin/bash) || exec /bin/sh
    # root         246  0.0  0.0   2576   112 pts/2    S+   15:20   0:00  \_ /bin/sh -c TERM=xterm-256color; export TERM; [ -x /bin/bash ] && ([ -x /usr/bin/script ] && /usr/bin/script -q -c "/bin/bash" /dev/null || exec /bin/bash) || exec /bin/sh
    # root         247  0.0  0.0   2936  1056 pts/2    S+   15:20   0:00      \_ /usr/bin/script -q -c /bin/bash /dev/null
    # root         248  0.0  0.0   2576   928 pts/3    Ss   15:20   0:00          \_ sh -c /bin/bash
    # root         249  0.0  0.0   4608  3796 pts/3    S    15:20   0:00              \_ /bin/bash
    # root         267  0.0  0.0   8480  4160 pts/3    R+   15:22   0:00                  \_ ps uxf
    # root         216  0.0  0.0   2576   932 pts/0    Ss+  15:20   0:00 /bin/sh -c TERM=xterm-256color; export TERM; [ -x /bin/bash ] && ([ -x /usr/bin/script ] && /usr/bin/script -q -c "/bin/bash" /dev/null || exec /bin/bash) || exec /bin/sh
    # root         222  0.0  0.0   2576   112 pts/0    S+   15:20   0:00  \_ /bin/sh -c TERM=xterm-256color; export TERM; [ -x /bin/bash ] && ([ -x /usr/bin/script ] && /usr/bin/script -q -c "/bin/bash" /dev/null || exec /bin/bash) || exec /bin/sh
    # root         223  0.0  0.0   2936  1052 pts/0    S+   15:20   0:00      \_ /usr/bin/script -q -c /bin/bash /dev/null
    # root         224  0.0  0.0   2576   936 pts/1    Ss   15:20   0:00          \_ sh -c /bin/bash
    # root         225  0.0  0.0   4608  3724 pts/1    S+   15:20   0:00              \_ /bin/bash
    # root         170  0.0  0.0   2576   928 ?        Ss   15:19   0:00 /bin/sh -c /opt/docker-entrypoint.sh $0 $@ run_strategy_updater
    # root         176  0.0  0.0   4344  3108 ?        S    15:19   0:00  \_ /bin/bash /opt/docker-entrypoint.sh run_strategy_updater
    # root         177  0.0  0.0  12332 10624 ?        S    15:19
    logs = subprocess.run("ps uxf | grep 'python -m tq_service_strategy.engine' | grep -v grep | awk '{ print $2 }' | xargs --no-run-if-empty -I@ kill -HUP @", shell=True)
    logger.info(f"Strategy restart logs {logs}.")


if __name__ == "__main__":

    logger.info("Running updater for the first time.")
    updater()

    while tq_infra_config_loader.EXT_SCRIPTS_RUN_FOREVER:
        logger.info(f"Sleeping for {tq_infra_config_loader.EXT_SCRIPTS_POLL_INTERVAL} seconds.")
        time.sleep(tq_infra_config_loader.EXT_SCRIPTS_POLL_INTERVAL)

        logger.info("Running updater.")
        updater()
