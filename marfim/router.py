"""Model router — one router that is actually used (review §4.1).

Roles (Oh My Pi doctrine): default/plan/commit/task/smol/vision resolve to an
ORDERED list of `provider/model` candidates from `config.model_roles`
(comma-separated fallbacks) falling back to default_provider/default_model.

Failover: a candidate that fails with 429/5xx/timeout (retry-exhausted
ProviderError) hands off to the next candidate — this is how opencode, Hermes,
Grok and Claude-class backends become one workspace.

Capabilities: a provider-neutral record (tool calling, context window, price)
resolved from the models.dev catalog, so the loop never sends tools to a model
that cannot take them.

Cost caps: `config.role_budgets[role]` (dollars) — a role whose accumulated
spend exceeds its cap refuses further calls instead of silently billing.
"""

from __future__ import annotations

import threading
from dataclasses import dataclass

from marfim.config import Config
from marfim.providers.base import Provider, ProviderError, build_provider


@dataclass
class Capabilities:
    tool_call: bool = True
    context_window: int = 200_000
    cost_in: float = 0.0
    cost_out: float = 0.0
    free: bool = False


class BudgetExceeded(ProviderError):
    def __init__(self, role: str, spent: float, cap: float):
        super().__init__(f"role '{role}' budget exceeded (${spent:.4f} >= ${cap:.2f}); "
                         f"raise role_budgets['{role}'] in the global config",
                         kind="budget")
        self.role = role


class Router:
    """Resolves role -> ordered provider/model candidates with failover."""

    def __init__(self, config: Config):
        self.config = config
        self._lock = threading.Lock()
        self.spend: dict[str, float] = {}
        self._providers: dict[tuple[str, str], Provider] = {}

    # ------------------------------------------------------------- resolving

    def candidates(self, role: str) -> list[tuple[str, str]]:
        """Ordered (provider_name, model) candidates for a role."""
        out: list[tuple[str, str]] = []
        spec = (self.config.model_roles.get(role) or "").strip() if role != "default" else ""
        if not spec:
            spec = (self.config.model_roles.get("default") or "").strip()
        for part in (spec.split(",") if spec else []):
            part = part.strip()
            if not part:
                continue
            out.append(self._split_spec(part))
        default = (self.config.default_provider, self.config.default_model)
        if (not out or out[-1] != default) and default not in out:
            out.append(default)
        return out

    def _split_spec(self, spec: str) -> tuple[str, str]:
        if "/" in spec:
            prov, _, model = spec.partition("/")
            if self._is_provider(prov):
                return prov, model
        # bare model id or provider-less catalog id: keep current provider
        return self.config.default_provider, spec

    def _is_provider(self, name: str) -> bool:
        from marfim.providers.registry import get_provider_preset
        return name in self.config.providers or get_provider_preset(name) is not None

    # ----------------------------------------------------------- capabilities

    def capabilities(self, provider_name: str, model: str) -> Capabilities:
        """Provider-neutral capability record (never send tools/images to a
        model that cannot take them)."""
        try:
            from marfim.providers.registry import list_catalog_models
            from marfim.config import marfim_home
            models = list_catalog_models(provider_name, fetch=False)
            for m in models:
                if m.model == model:
                    return Capabilities(tool_call=m.tool_call,
                                        context_window=m.context_window or 200_000,
                                        cost_in=m.cost_in, cost_out=m.cost_out,
                                        free=m.free)
        except Exception:
            pass
        return Capabilities()

    def context_window(self, provider_name: str, model: str) -> int:
        return self.capabilities(provider_name, model).context_window

    # ------------------------------------------------------------------ spend

    def add_spend(self, role: str, provider_name: str, model: str,
                  input_tokens: int, output_tokens: int) -> float:
        caps = self.capabilities(provider_name, model)
        cost = (input_tokens * caps.cost_in + output_tokens * caps.cost_out) / 1_000_000
        with self._lock:
            self.spend[role] = self.spend.get(role, 0.0) + cost
        return cost

    def check_budget(self, role: str) -> None:
        cap = self.config.role_budgets.get(role)
        if cap is None:
            return
        spent = self.spend.get(role, 0.0)
        if spent >= cap:
            raise BudgetExceeded(role, spent, float(cap))

    # ----------------------------------------------------------------- stream

    def provider_for(self, provider_name: str, model: str) -> Provider:
        key = (provider_name, model)
        with self._lock:
            prov = self._providers.get(key)
            if prov is None:
                prov = build_provider(self.config.provider(provider_name), model)
                self._providers[key] = prov
            return prov

    def stream_with_failover(self, messages: list, tools: list, system: str,
                             role: str = "default"):
        """Yield events from the first healthy candidate; 429/5xx/timeout
        failover to the next (review §4.1).  Budget is checked per role."""
        self.check_budget(role)
        candidates = self.candidates(role)
        last_error: ProviderError | None = None
        for provider_name, model in candidates:
            caps = self.capabilities(provider_name, model)
            if tools and not caps.tool_call:
                continue    # never send tools to a model that cannot take them
            effective_tools = tools if caps.tool_call else []
            prov = self.provider_for(provider_name, model)
            from marfim.providers.base import retry_iterator
            try:
                events = retry_iterator(
                    lambda p=prov, t=effective_tools: p.stream(messages, t, system=system),
                    retry_max=self.config.retry_max)
                yield from events
                return
            except ProviderError as exc:
                retryable = exc.kind in ("retry_exhausted", "transport") or \
                    exc.status in (429, 500, 502, 503, 504)
                if not retryable:
                    raise
                last_error = exc
                continue
        raise last_error or ProviderError(
            f"no capable provider/model for role '{role}' "
            f"(candidates: {candidates})", kind="no-candidate")
