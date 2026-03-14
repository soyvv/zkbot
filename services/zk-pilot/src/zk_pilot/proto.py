"""Proto message definitions for zk.pilot.v1 bootstrap protocol.

Uses betterproto dataclasses — wire-compatible with the protobuf definitions in
`protos/zk/pilot/v1/bootstrap.proto`. No codegen step required.
"""

import betterproto


class BootstrapRegisterRequest(betterproto.Message):
    token: str = betterproto.string_field(1)
    logical_id: str = betterproto.string_field(2)
    instance_type: str = betterproto.string_field(3)
    env: str = betterproto.string_field(4)
    runtime_info: dict[str, str] = betterproto.map_field(5, betterproto.TYPE_STRING, betterproto.TYPE_STRING)


class BootstrapRegisterResponse(betterproto.Message):
    owner_session_id: str = betterproto.string_field(1)
    kv_key: str = betterproto.string_field(2)
    lock_key: str = betterproto.string_field(3)
    lease_ttl_ms: int = betterproto.int64_field(4)
    instance_id: int = betterproto.int32_field(5)
    scoped_credential: str = betterproto.string_field(6)
    status: str = betterproto.string_field(7)
    error_message: str = betterproto.string_field(8)


class BootstrapDeregisterRequest(betterproto.Message):
    owner_session_id: str = betterproto.string_field(1)
    logical_id: str = betterproto.string_field(2)
    instance_type: str = betterproto.string_field(3)
    env: str = betterproto.string_field(4)


class BootstrapDeregisterResponse(betterproto.Message):
    success: bool = betterproto.bool_field(1)
    error_message: str = betterproto.string_field(2)
