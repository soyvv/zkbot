import json

import betterproto
import nats
from nats.errors import TimeoutError
from loguru import logger

async def _nc_request_with_retry(nc: nats.NATS, subject, headers, payload,
                                 max_retries=3, initial_timeout=2, backoff_factor=2):
    retry = 0
    timeout = initial_timeout
    while retry < max_retries:
        try:
            resp = await nc.request(
                subject=subject, headers=headers, payload=payload, timeout=timeout)
            return resp
        except TimeoutError as e:
            retry += 1
            timeout = min(timeout * backoff_factor, 10)
            logger.warning(f"error in request: {e}")
            logger.warning(f"retrying request {retry} with timeout {timeout}")

    raise TimeoutError(f"request to {subject} failed after {max_retries} retries")


async def rpc(nc: nats.NATS,
              subject: str,
              method: str,
              request_payload: any,
              response_type: any,
              timeout_in_secs=2,
              retry_on_timeout=False) -> tuple[any, bool]:
    '''
    perform an RPC call to tq-rpc wrapped service
    :param nc: nats client
    :param subject: NATS subject to send the request
    :param method: method name (convention: CamelCase)
    :param request_payload: payload to send; pb or string
    :param response_type: type of response to expect
    :param timeout_in_secs: timeout in seconds
    :param retry_on_timeout: if True, will retry 3 times with backoff
    :return: tuple of (response, is_error); response is either pb or string or bytes
    '''
    return await _rpc(nc=nc, subject=subject, method=method,
                      payload=request_payload,
                      return_type=response_type,
                      timeout_in_secs=timeout_in_secs, retry_on_timeout=retry_on_timeout)


async def _rpc(nc: nats.NATS, subject: str, method: str, payload: any, return_type: any,
               timeout_in_secs=2, retry_on_timeout=False) -> tuple[any, bool]:
    headers = {"rpc_method": method}
    if isinstance(payload, str):
        payload_bytes = bytes(payload, encoding='utf-8')
    else:
        payload_bytes = bytes(payload)
    if retry_on_timeout:
        resp = await _nc_request_with_retry(nc, subject, headers, payload_bytes, max_retries=3,
                                            initial_timeout=timeout_in_secs if timeout_in_secs < 10 else 10)
    else:
        resp = await nc.request(subject=subject, headers=headers, payload=payload_bytes, timeout=timeout_in_secs)

    if resp.headers and resp.headers.get("error"):
        is_error = True
        rpc_error_msg = str(resp.data, encoding='utf-8')
        rpc_error = json.dumps(rpc_error_msg)
        return (rpc_error, is_error)
    else:
        if issubclass(return_type, betterproto.Message):
            return return_type().parse(resp.data), False
        elif isinstance(return_type, str):
            return resp.data.decode('utf-8'), False
        else:
            return resp.data, False