"""Agent read/config domain callables (Rust-owned routes).

Messaging, streaming and action-plan confirmation remain on the legacy
passthrough until their async/streaming semantics migrate.
"""

from __future__ import annotations

from typing import Any

from ..domain import DomainContext, DomainResult, register, ok
from ..schemas import AgentThreadCreate, AgentThreadUpdate
from . import as_int


@register("agent.providers.list")
def agent_providers_list(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    return ok(ctx.agent.list_providers())


@register("agent.threads.list")
def agent_threads_list(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    course_id = as_int((query or {}).get("course_id"), "course_id")
    return ok(ctx.agent.list_threads(course_id))


@register("agent.threads.create")
def agent_threads_create(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    payload = AgentThreadCreate(**(body or {}))
    values = payload.model_dump()
    return ok(ctx.agent.create_thread(values.pop("course_id"), values))


@register("agent.threads.get")
def agent_threads_get(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    return ok(
        ctx.agent.get_thread(as_int((path or {}).get("thread_id"), "thread_id"))
    )


@register("agent.threads.update")
def agent_threads_update(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    payload = AgentThreadUpdate(**(body or {}))
    return ok(
        ctx.agent.update_thread(
            as_int((path or {}).get("thread_id"), "thread_id"),
            payload.model_dump(exclude_unset=True, exclude_none=True),
        )
    )


@register("agent.threads.delete")
def agent_threads_delete(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    ctx.agent.delete_thread(as_int((path or {}).get("thread_id"), "thread_id"))
    return DomainResult(status=204)


@register("agent.providers.update")
def agent_providers_update(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    from ..schemas import AgentProviderUpdate

    payload = AgentProviderUpdate(**(body or {}))
    return ok(
        ctx.agent.configure_provider(
            (path or {}).get("provider_id", ""),
            payload.model_dump(exclude_none=True),
        )
    )


@register("agent.providers.delete")
def agent_providers_delete(
    ctx: DomainContext, path: dict[str, Any] | None = None,
    query: dict[str, Any] | None = None, body: Any = None,
) -> DomainResult:
    ctx.agent.delete_provider((path or {}).get("provider_id", ""))
    return DomainResult(status=204)
