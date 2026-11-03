"""Model catalog + provider presets.

The catalog syncs https://models.dev/api.json (the same public catalog opencode
v2 uses) with a 24h disk cache and offline fallback — free models are surfaced
first.  Provider presets cover OpenAI-compatible and Anthropic wire protocols
with editable base URLs (Codex `model_providers` doctrine).
"""

from __future__ import annotations

import os
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path

from marfim.config import marfim_home
from marfim.util import json_loads

CATALOG_URL = "https://models.dev/api.json"
CATALOG_TTL = 24 * 3600

PRESETS: dict[str, dict] = {
    # name: {base_url, wire, api_key_env, default_model}
    "openai":     {"base_url": "https://api.openai.com/v1", "wire": "openai", "api_key_env": "OPENAI_API_KEY", "default_model": "gpt-4.1-mini"},
    "anthropic":  {"base_url": "https://api.anthropic.com", "wire": "anthropic", "api_key_env": "ANTHROPIC_API_KEY", "default_model": "claude-sonnet-4-5"},
    "openrouter": {"base_url": "https://openrouter.ai/api/v1", "wire": "openai", "api_key_env": "OPENROUTER_API_KEY", "default_model": "openrouter/free"},
    "groq":       {"base_url": "https://api.groq.com/openai/v1", "wire": "openai", "api_key_env": "GROQ_API_KEY", "default_model": "llama-3.3-70b-versatile"},
    "deepseek":   {"base_url": "https://api.deepseek.com/v1", "wire": "openai", "api_key_env": "DEEPSEEK_API_KEY", "default_model": "deepseek-chat"},
    "together":   {"base_url": "https://api.together.xyz/v1", "wire": "openai", "api_key_env": "TOGETHER_API_KEY", "default_model": "meta-llama/Llama-3.3-70B-Instruct-Turbo"},
    "mistral":    {"base_url": "https://api.mistral.ai/v1", "wire": "openai", "api_key_env": "MISTRAL_API_KEY", "default_model": "mistral-large-latest"},
    "xai":        {"base_url": "https://api.x.ai/v1", "wire": "openai", "api_key_env": "XAI_API_KEY", "default_model": "grok-3-mini"},
    "moonshot":   {"base_url": "https://api.moonshot.cn/v1", "wire": "openai", "api_key_env": "MOONSHOT_API_KEY", "default_model": "kimi-k2-0905-preview"},
    "zhipu":      {"base_url": "https://open.bigmodel.cn/api/paas/v4", "wire": "openai", "api_key_env": "ZHIPU_API_KEY", "default_model": "glm-4.6"},
    "google":     {"base_url": "https://generativelanguage.googleapis.com/v1beta/openai", "wire": "openai", "api_key_env": "GEMINI_API_KEY", "default_model": "gemini-2.5-flash"},
    "fireworks":  {"base_url": "https://api.fireworks.ai/inference/v1", "wire": "openai", "api_key_env": "FIREWORKS_API_KEY", "default_model": "accounts/fireworks/models/llama4-maverick-instruct-basic"},
    "cerebras":   {"base_url": "https://api.cerebras.ai/v1", "wire": "openai", "api_key_env": "CEREBRAS_API_KEY", "default_model": "llama-3.3-70b"},
    "perplexity": {"base_url": "https://api.perplexity.ai", "wire": "openai", "api_key_env": "PERPLEXITY_API_KEY", "default_model": "sonar-pro"},
    "novita":     {"base_url": "https://api.novita.ai/v3/openai", "wire": "openai", "api_key_env": "NOVITA_API_KEY", "default_model": "deepseek/deepseek-v3"},
    "chutes":     {"base_url": "https://llm.chutes.ai/v1", "wire": "openai", "api_key_env": "CHUTES_API_KEY", "default_model": "deepseek-ai/DeepSeek-V3"},
    "ollama":     {"base_url": "http://localhost:11434/v1", "wire": "openai", "api_key_env": "", "default_model": "qwen3-coder:30b"},
    "lmstudio":   {"base_url": "http://localhost:1234/v1", "wire": "openai", "api_key_env": "", "default_model": "local-model"},
    "vllm":       {"base_url": "http://localhost:8000/v1", "wire": "openai", "api_key_env": "", "default_model": "local-model"},
    "mock":       {"base_url": "mock://local", "wire": "mock", "api_key_env": "", "default_model": "mock-1"},
}


def get_provider_preset(name: str) -> dict | None:
    preset = PRESETS.get(name)
    if not preset:
        return None
    return {"name": name, **preset}


def get_preset(name: str) -> dict | None:
    """Alias kept for readability in doctor/cli."""
    return get_provider_preset(name)


@dataclass
class ModelInfo:
    provider: str
    model: str
    name: str = ""
    context_window: int = 0
    cost_in: float = 0.0
    cost_out: float = 0.0
    free: bool = False
    tool_call: bool = True

    def price_label(self) -> str:
        if self.free:
            return "free"
        parts = []
        if self.cost_in:
            parts.append(f"in ${self.cost_in:.2f}/M")
        if self.cost_out:
            parts.append(f"out ${self.cost_out:.2f}/M")
        return " ".join(parts) or "n/a"


def catalog_cache_path() -> Path:
    return marfim_home() / "cache" / "models-dev.json"


def fetch_catalog(force: bool = False, timeout: int = 15) -> dict | None:
    """Return the models.dev catalog dict, using a 24h disk cache (opencode
    doctrine: KV cache + offline snapshot).  Returns None when offline and no
    cache exists."""
    cache = catalog_cache_path()
    if not force and cache.is_file():
        age = time.time() - cache.stat().st_mtime
        if age < CATALOG_TTL:
            data = json_loads(cache.read_text(encoding="utf-8"))
            if isinstance(data, dict):
                return data
    try:
        req = urllib.request.Request(CATALOG_URL, headers={"User-Agent": "marfim/0.1"})
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            data = resp.read().decode("utf-8", "replace")
        parsed = __import__("json").loads(data)
        if isinstance(parsed, dict):
            cache.parent.mkdir(parents=True, exist_ok=True)
            cache.write_text(data, encoding="utf-8")
            return parsed
    except Exception:
        pass
    if cache.is_file():
        return json_loads(cache.read_text(encoding="utf-8"))
    return None


def list_catalog_models(provider: str | None = None) -> list[ModelInfo]:
    cat = fetch_catalog() or {}
    out: list[ModelInfo] = []
    for prov_name, prov in cat.items():
        if provider and prov_name != provider:
            continue
        for model_id, meta in (prov.get("models") or {}).items():
            if not isinstance(meta, dict):
                continue
            cost = meta.get("cost") or {}
            cost_in = float(cost.get("input") or 0)
            cost_out = float(cost.get("output") or 0)
            out.append(ModelInfo(
                provider=prov_name, model=model_id, name=str(meta.get("name") or model_id),
                context_window=int(meta.get("limit", {}).get("context") or 0),
                cost_in=cost_in, cost_out=cost_out,
                free=(cost_in == 0 and cost_out == 0),
                tool_call=bool((meta.get("tool_call") is not False)),
            ))
    return out


def free_models(limit: int = 40) -> list[ModelInfo]:
    """Zero-cost models first — opencode's free-model onboarding doctrine."""
    models = [m for m in list_catalog_models() if m.free and m.tool_call]
    models.sort(key=lambda m: (m.provider, m.model))
    return models[:limit]


def model_price(provider: str, model: str) -> tuple[float, float]:
    for m in list_catalog_models(provider):
        if m.model == model:
            return m.cost_in, m.cost_out
    return 0.0, 0.0


def has_api_key(provider_name: str) -> bool:
    preset = get_provider_preset(provider_name)
    if not preset:
        return True  # custom provider: assume configured
    env_name = preset.get("api_key_env", "")
    if not env_name:
        return True  # local providers need no key
    return bool(os.environ.get(env_name))
