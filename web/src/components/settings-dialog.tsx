import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { RefreshCcw, Plus, X } from "lucide-react";

import { api } from "../lib/api";
import { newAlertId } from "../lib/app-utils";
import { cn } from "../lib/utils";
import type {
  ConfigProviderSummary,
  ConfigProvidersResponse,
  ConfigRuntimeResponse,
} from "../lib/types";
import type { AlertItem } from "./alert-stack";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "./ui/dialog";
import { Input } from "./ui/input";
import { Textarea } from "./ui/textarea";
import { Badge } from "./ui/badge";

type ProviderDraft = {
  id: string;
  api: string;
  baseUrl: string;
  models: string[];
  apiKeySet: boolean;
  apiKeyDraft: string;
  modelDraft: string;
};

type SettingsTab = "providers" | "agents" | "yaml";

const PROVIDERS_PANE_DEFAULT_MODEL = "__default_model__";
const PROVIDERS_PANE_NEW_PROVIDER = "__new_provider__";

export function SettingsDialog(props: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onApiError: (err: unknown) => void;
  onConfigChanged: () => Promise<void>;
  pushAlert: (alert: AlertItem) => void;
}) {
  const { open, onOpenChange, onApiError, onConfigChanged, pushAlert } = props;
  const onApiErrorRef = useRef(onApiError);
  onApiErrorRef.current = onApiError;

  const [settingsTab, setSettingsTab] = useState<SettingsTab>("providers");
  const [settingsLoading, setSettingsLoading] = useState(false);
  const [settingsSaving, setSettingsSaving] = useState(false);

  const [configYaml, setConfigYaml] = useState("");
  const [configPath, setConfigPath] = useState("");
  const [configLoaded, setConfigLoaded] = useState(false);
  const [savingConfig, setSavingConfig] = useState(false);

  const [settingsProvidersDefaultModel, setSettingsProvidersDefaultModel] = useState("");
  const [settingsProviders, setSettingsProviders] = useState<ProviderDraft[]>([]);
  const [providersPaneSelection, setProvidersPaneSelection] = useState<string>("");

  const [newProviderId, setNewProviderId] = useState("");
  const [newProviderBaseUrl, setNewProviderBaseUrl] = useState("");
  const [newProviderModels, setNewProviderModels] = useState<string[]>([]);
  const [newProviderModelDraft, setNewProviderModelDraft] = useState("");
  const [newProviderApiKey, setNewProviderApiKey] = useState("");

  const [runtimeMaxSteps, setRuntimeMaxSteps] = useState("");
  const [agentsPlanMaxSteps, setAgentsPlanMaxSteps] = useState("");
  const [agentsGeneralMaxSteps, setAgentsGeneralMaxSteps] = useState("");

  const [providerDeleteConfirm, setProviderDeleteConfirm] = useState<{
    providerId: string;
  } | null>(null);

  const providersPaneSelectedProvider = useMemo(() => {
    return settingsProviders.find((p) => p.id === providersPaneSelection) ?? null;
  }, [settingsProviders, providersPaneSelection]);

  const defaultModelSuggestions = useMemo(() => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const p of settingsProviders) {
      for (const m of p.models ?? []) {
        const trimmed = (m ?? "").trim();
        if (!trimmed) continue;
        const qualified = trimmed.includes("/") ? trimmed : `${p.id}/${trimmed}`;
        if (seen.has(qualified)) continue;
        seen.add(qualified);
        out.push(qualified);
      }
    }
    out.sort();
    return out;
  }, [settingsProviders]);

  const resetDrafts = useCallback(() => {
    setSettingsTab("providers");
    setConfigYaml("");
    setConfigPath("");
    setConfigLoaded(false);
    setNewProviderId("");
    setNewProviderBaseUrl("");
    setNewProviderModels([]);
    setNewProviderModelDraft("");
    setNewProviderApiKey("");
    setProviderDeleteConfirm(null);
  }, []);

  function normalizeModels(models: string[]): string[] {
    const seen = new Set<string>();
    const out: string[] = [];
    for (const raw of models ?? []) {
      const v = (raw ?? "").trim();
      if (!v) continue;
      if (seen.has(v)) continue;
      seen.add(v);
      out.push(v);
    }
    return out;
  }

  function parseModelTokens(raw: string): string[] {
    if (!raw) return [];
    return normalizeModels(raw.split(/[\r\n,]+/g));
  }

  function normalizeProviderDraft(p: ConfigProviderSummary): ProviderDraft {
    return {
      id: p.id,
      api: p.api || "openai_chat_completions",
      baseUrl: p.base_url ?? "",
      models: normalizeModels(p.models ?? []),
      apiKeySet: Boolean(p.api_key_set),
      apiKeyDraft: "",
      modelDraft: "",
    };
  }

  const loadSettingsProviders = useCallback(async () => {
    const res: ConfigProvidersResponse = await api.getConfigProviders();
    setSettingsProvidersDefaultModel(res.default_model ?? "");
    const providers = (res.providers ?? []).map(normalizeProviderDraft);
    providers.sort((a, b) => a.id.localeCompare(b.id));
    setSettingsProviders(providers);
    setProvidersPaneSelection((prev) => {
      const cur = (prev ?? "").trim();
      if (cur === PROVIDERS_PANE_DEFAULT_MODEL || cur === PROVIDERS_PANE_NEW_PROVIDER) {
        return cur;
      }
      if (cur && providers.some((p) => p.id === cur)) return cur;
      if (providers.length) return providers[0].id;
      return PROVIDERS_PANE_NEW_PROVIDER;
    });
  }, []);

  const loadSettingsRuntime = useCallback(async () => {
    const res: ConfigRuntimeResponse = await api.getConfigRuntime();
    setRuntimeMaxSteps(res.runtime_max_steps != null ? String(res.runtime_max_steps) : "");
    setAgentsPlanMaxSteps(
      res.agents_plan_max_steps != null ? String(res.agents_plan_max_steps) : "",
    );
    setAgentsGeneralMaxSteps(
      res.agents_general_max_steps != null ? String(res.agents_general_max_steps) : "",
    );
  }, []);

  const ensureConfigLoaded = useCallback(async (force = false) => {
    if (!force && configLoaded) return;
    const cfg = await api.getConfig();
    setConfigYaml(cfg.yaml);
    setConfigPath(cfg.path);
    setConfigLoaded(true);
  }, [configLoaded]);

  useEffect(() => {
    if (!open) return;
    resetDrafts();

    let cancelled = false;
    setSettingsLoading(true);
    Promise.all([loadSettingsProviders(), loadSettingsRuntime()])
      .catch((err) => onApiErrorRef.current(err))
      .finally(() => {
        if (!cancelled) setSettingsLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [open, loadSettingsProviders, loadSettingsRuntime, resetDrafts]);

  function setProviderDraft(id: string, patch: Partial<ProviderDraft>) {
    setSettingsProviders((prev) => prev.map((p) => (p.id === id ? { ...p, ...patch } : p)));
  }

  function addModelsToProvider(id: string, raw: string) {
    const tokens = parseModelTokens(raw);
    if (!tokens.length) return;
    setSettingsProviders((prev) =>
      prev.map((p) =>
        p.id === id
          ? { ...p, models: normalizeModels([...p.models, ...tokens]), modelDraft: "" }
          : p,
      ),
    );
  }

  function removeModelFromProvider(id: string, model: string) {
    setSettingsProviders((prev) =>
      prev.map((p) => (p.id === id ? { ...p, models: p.models.filter((m) => m !== model) } : p)),
    );
  }

  function addModelsToNewProvider(raw: string) {
    const tokens = parseModelTokens(raw);
    if (!tokens.length) return;
    setNewProviderModels((prev) => normalizeModels([...prev, ...tokens]));
    setNewProviderModelDraft("");
  }

  function removeModelFromNewProvider(model: string) {
    setNewProviderModels((prev) => prev.filter((m) => m !== model));
  }

  async function saveProvidersDefaultModel() {
    const trimmed = settingsProvidersDefaultModel.trim();
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ default_model: trimmed ? trimmed : null });
      await onConfigChanged();
      pushAlert({
        id: newAlertId("cfg"),
        level: "success",
        title: "Saved",
        message: "Default model updated.",
        autoCloseMs: 2500,
      });
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function clearProvidersDefaultModel() {
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ default_model: null });
      setSettingsProvidersDefaultModel("");
      await onConfigChanged();
      pushAlert({
        id: newAlertId("cfg"),
        level: "success",
        title: "Saved",
        message: "Default model cleared.",
        autoCloseMs: 2500,
      });
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function addProvider() {
    const id = newProviderId.trim();
    const baseUrl = newProviderBaseUrl.trim();
    const models = normalizeModels(newProviderModels);
    const apiKey = newProviderApiKey.trim();

    if (!id) {
      pushAlert({
        id: newAlertId("cfg"),
        level: "error",
        title: "Invalid provider",
        message: "Provider id is required.",
      });
      return;
    }
    if (!baseUrl) {
      pushAlert({
        id: newAlertId("cfg"),
        level: "error",
        title: "Invalid provider",
        message: "Base URL is required.",
      });
      return;
    }

    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({
        upsert: [
          {
            id,
            api: "openai_chat_completions",
            base_url: baseUrl,
            models,
            api_key: apiKey ? apiKey : undefined,
          },
        ],
      });
      setProvidersPaneSelection(id);
      await loadSettingsProviders();
      await onConfigChanged();
      setNewProviderId("");
      setNewProviderBaseUrl("");
      setNewProviderModels([]);
      setNewProviderModelDraft("");
      setNewProviderApiKey("");
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function saveProvider(id: string) {
    const p = settingsProviders.find((v) => v.id === id);
    if (!p) return;
    const baseUrl = p.baseUrl.trim();
    if (!baseUrl) {
      pushAlert({
        id: newAlertId("cfg"),
        level: "error",
        title: "Invalid provider",
        message: "Base URL is required.",
      });
      return;
    }
    const models = normalizeModels(p.models);

    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({
        upsert: [{ id, api: p.api, base_url: baseUrl, models }],
      });
      setProviderDraft(id, { baseUrl, models });
      await onConfigChanged();
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function updateProviderApiKey(id: string) {
    const p = settingsProviders.find((v) => v.id === id);
    if (!p) return;
    const key = p.apiKeyDraft.trim();
    if (!key) return;

    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ upsert: [{ id, api_key: key }] });
      setProviderDraft(id, { apiKeyDraft: "", apiKeySet: true });
      await onConfigChanged();
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function clearProviderApiKey(id: string) {
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ upsert: [{ id, api_key: null }] });
      setProviderDraft(id, { apiKeyDraft: "", apiKeySet: false });
      await onConfigChanged();
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function deleteProvider(id: string) {
    setSettingsSaving(true);
    try {
      await api.patchConfigProviders({ delete: [id] });
      const remaining = settingsProviders.filter((p) => p.id !== id);
      setSettingsProviders(remaining);
      setProvidersPaneSelection((prev) => {
        if (prev !== id) return prev;
        return remaining[0]?.id ?? PROVIDERS_PANE_NEW_PROVIDER;
      });
      await onConfigChanged();
      pushAlert({
        id: newAlertId("cfg"),
        level: "success",
        title: "Deleted",
        message: `Provider "${id}" deleted.`,
        autoCloseMs: 3000,
      });
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  function parseOptionalPositiveInt(
    label: string,
    raw: string,
  ): number | null | "invalid" {
    const trimmed = raw.trim();
    if (!trimmed) return null;
    const n = Number(trimmed);
    if (!Number.isFinite(n) || !Number.isInteger(n) || n <= 0) {
      pushAlert({
        id: newAlertId("cfg"),
        level: "error",
        title: "Invalid value",
        message: `${label} must be a positive integer.`,
        autoCloseMs: 4000,
      });
      return "invalid";
    }
    return n;
  }

  async function saveRuntimeConfig() {
    const runtime = parseOptionalPositiveInt("runtime.max_steps", runtimeMaxSteps);
    const plan = parseOptionalPositiveInt("agents.plan.max_steps", agentsPlanMaxSteps);
    const general = parseOptionalPositiveInt("agents.general.max_steps", agentsGeneralMaxSteps);
    if (runtime === "invalid" || plan === "invalid" || general === "invalid") return;

    setSettingsSaving(true);
    try {
      await api.patchConfigRuntime({
        runtime_max_steps: runtime,
        agents_plan_max_steps: plan,
        agents_general_max_steps: general,
      });
      await onConfigChanged();
    } catch (err) {
      onApiError(err);
    } finally {
      setSettingsSaving(false);
    }
  }

  async function saveConfig() {
    setSavingConfig(true);
    try {
      await api.putConfig({ yaml: configYaml });
      pushAlert({
        id: newAlertId("config"),
        level: "success",
        title: "Saved",
        message: "Config updated.",
        autoCloseMs: 2500,
      });
      await Promise.all([loadSettingsProviders(), loadSettingsRuntime()]);
      await onConfigChanged();
    } catch (err) {
      onApiError(err);
    } finally {
      setSavingConfig(false);
    }
  }

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="md:w-[min(1080px,calc(100vw-24px))] md:max-w-none">
          <DialogHeader>
            <DialogTitle>Settings</DialogTitle>
            <DialogDescription className="truncate">
              Global configuration (kiliax.yaml)
            </DialogDescription>
          </DialogHeader>
          <div className="mb-3 flex items-center gap-2">
            <div className="inline-flex rounded-md border border-zinc-200 bg-white p-1">
              <button
                className={cn(
                  "rounded px-3 py-1 text-xs",
                  settingsTab === "providers"
                    ? "bg-blue-50 text-blue-700"
                    : "text-zinc-700 hover:bg-zinc-50",
                )}
                onClick={() => setSettingsTab("providers")}
              >
                Providers
              </button>
              <button
                className={cn(
                  "rounded px-3 py-1 text-xs",
                  settingsTab === "agents"
                    ? "bg-blue-50 text-blue-700"
                    : "text-zinc-700 hover:bg-zinc-50",
                )}
                onClick={() => setSettingsTab("agents")}
              >
                Agents
              </button>
              <button
                className={cn(
                  "rounded px-3 py-1 text-xs",
                  settingsTab === "yaml"
                    ? "bg-blue-50 text-blue-700"
                    : "text-zinc-700 hover:bg-zinc-50",
                )}
                onClick={() => {
                  setSettingsTab("yaml");
                  ensureConfigLoaded().catch(onApiError);
                }}
              >
                Raw YAML
              </button>
            </div>
            <div className="flex-1" />
            <Button
              variant="outline"
              size="icon"
              className="h-8 w-8"
              aria-label="Reload settings"
              title="Reload settings"
              disabled={settingsLoading || settingsSaving || savingConfig}
              onClick={async () => {
                setSettingsLoading(true);
                try {
                  await Promise.all([loadSettingsProviders(), loadSettingsRuntime()]);
                  if (settingsTab === "yaml") {
                    await ensureConfigLoaded(true);
                  }
                } catch (err) {
                  onApiError(err);
                } finally {
                  setSettingsLoading(false);
                }
              }}
            >
              <RefreshCcw className="h-4 w-4 text-zinc-600" />
            </Button>
          </div>

          {settingsLoading ? (
            <div className="py-10 text-center text-sm text-zinc-500">Loading…</div>
          ) : settingsTab === "providers" ? (
            <div className="flex h-[min(600px,72vh)] flex-col gap-3 md:flex-row">
              <div className="flex h-[min(240px,30vh)] w-full shrink-0 flex-col overflow-hidden rounded-lg border border-zinc-200 bg-white md:h-full md:w-80">
                <div className="flex items-center justify-between border-b border-zinc-200 px-3 py-2">
                  <div className="text-xs font-semibold text-zinc-700">Providers</div>
                  <div className="text-xs text-zinc-500">{settingsProviders.length}</div>
                </div>

                <div className="min-h-0 flex-1 overflow-auto p-2">
                  <div className="space-y-1">
                    <button
                      className={cn(
                        "w-full rounded-md border border-transparent px-2 py-2 text-left hover:border-zinc-200 hover:bg-zinc-50",
                        providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL
                          ? "border-blue-200 bg-blue-50"
                          : "",
                      )}
                      onClick={() => setProvidersPaneSelection(PROVIDERS_PANE_DEFAULT_MODEL)}
                    >
                      <div className="text-sm font-medium text-zinc-900">Default model</div>
                      <div className="mt-0.5 truncate text-xs text-zinc-500">
                        {settingsProvidersDefaultModel.trim() ? (
                          <span className="font-mono">{settingsProvidersDefaultModel.trim()}</span>
                        ) : (
                          "Not set"
                        )}
                      </div>
                    </button>

                    <button
                      className={cn(
                        "w-full rounded-md border border-transparent px-2 py-2 text-left hover:border-zinc-200 hover:bg-zinc-50",
                        providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER
                          ? "border-blue-200 bg-blue-50"
                          : "",
                      )}
                      onClick={() => setProvidersPaneSelection(PROVIDERS_PANE_NEW_PROVIDER)}
                    >
                      <div className="flex items-center gap-2 text-sm font-medium text-zinc-900">
                        <Plus className="h-4 w-4 text-violet-600" />
                        Add provider
                      </div>
                      <div className="mt-0.5 truncate text-xs text-zinc-500">
                        Create a new OpenAI-compatible provider.
                      </div>
                    </button>
                  </div>

                  <div className="my-2 border-t border-zinc-200" />

                  {settingsProviders.length ? (
                    <div className="space-y-1">
                      {settingsProviders.map((p) => (
                        <button
                          key={p.id}
                          className={cn(
                            "w-full rounded-md border border-transparent px-2 py-2 text-left hover:border-zinc-200 hover:bg-zinc-50",
                            providersPaneSelection === p.id ? "border-blue-200 bg-blue-50" : "",
                          )}
                          onClick={() => setProvidersPaneSelection(p.id)}
                        >
                          <div className="flex items-start justify-between gap-2">
                            <div className="min-w-0">
                              <div className="truncate text-sm font-medium text-zinc-900">
                                {p.id}
                              </div>
                              <div className="mt-0.5 truncate text-xs text-zinc-500">
                                {p.baseUrl || "—"}
                              </div>
                            </div>
                            <div className="flex shrink-0 flex-col items-end gap-1">
                              <Badge
                                variant={p.apiKeySet ? "done" : "idle"}
                                className="px-2 py-0.5 text-[11px]"
                              >
                                key
                              </Badge>
                              <div className="text-[11px] text-zinc-500">
                                {p.models.length} models
                              </div>
                            </div>
                          </div>
                        </button>
                      ))}
                    </div>
                  ) : (
                    <div className="px-2 py-6 text-center text-sm text-zinc-500">
                      No providers
                    </div>
                  )}
                </div>
              </div>

              <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-lg border border-zinc-200 bg-white">
                <div className="flex items-center justify-between border-b border-zinc-200 px-4 py-2">
                  <div className="min-w-0">
                    <div className="truncate text-sm font-semibold text-zinc-900">
                      {providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL
                        ? "Default model"
                        : providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER
                          ? "Add provider"
                          : providersPaneSelectedProvider?.id ?? "Provider"}
                    </div>
                    <div className="truncate text-xs text-zinc-500">
                      {providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL
                        ? "Used for new sessions when not overridden."
                        : providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER
                          ? "Base URL, models, and API key."
                          : providersPaneSelectedProvider?.baseUrl ??
                            "Select a provider on the left."}
                    </div>
                  </div>
                  {settingsSaving ? <div className="text-xs text-zinc-500">Saving…</div> : null}
                </div>

                <div className="min-h-0 flex-1 overflow-auto p-4">
                  {providersPaneSelection === PROVIDERS_PANE_DEFAULT_MODEL ? (
                    <div className="space-y-3">
                      <div className="text-xs text-zinc-600">Model id</div>
                      <Input
                        className="font-mono text-xs"
                        list="kiliax-model-suggestions"
                        placeholder="provider/model"
                        value={settingsProvidersDefaultModel}
                        onChange={(e) => setSettingsProvidersDefaultModel(e.target.value)}
                      />
                      <div className="flex justify-end gap-2">
                        <Button
                          variant="outline"
                          disabled={settingsSaving || !settingsProvidersDefaultModel.trim()}
                          onClick={clearProvidersDefaultModel}
                        >
                          Clear
                        </Button>
                        <Button onClick={saveProvidersDefaultModel} disabled={settingsSaving}>
                          Save
                        </Button>
                      </div>
                      <div className="text-xs text-zinc-500">
                        Example: <span className="font-mono">openai/gpt-4o-mini</span>
                      </div>
                    </div>
                  ) : providersPaneSelection === PROVIDERS_PANE_NEW_PROVIDER ? (
                    <div className="space-y-4">
                      <div className="grid gap-3 sm:grid-cols-2">
                        <div>
                          <div className="text-xs text-zinc-600">Provider id</div>
                          <Input
                            placeholder="openai"
                            value={newProviderId}
                            onChange={(e) => setNewProviderId(e.target.value)}
                          />
                        </div>
                        <div>
                          <div className="text-xs text-zinc-600">Base URL</div>
                          <Input
                            placeholder="https://api.openai.com/v1"
                            value={newProviderBaseUrl}
                            onChange={(e) => setNewProviderBaseUrl(e.target.value)}
                          />
                        </div>
                      </div>

                      <div>
                        <div className="flex items-center justify-between">
                          <div className="text-xs text-zinc-600">Models</div>
                          <div className="text-xs text-zinc-500">
                            {newProviderModels.length} total
                          </div>
                        </div>
                        <div className="mt-2 flex flex-wrap gap-2">
                          {newProviderModels.length ? (
                            newProviderModels.map((m) => (
                              <div
                                key={m}
                                className="flex min-w-0 max-w-full items-center gap-1 rounded-full border border-zinc-200 bg-zinc-50 px-2 py-1 text-xs"
                              >
                                <span className="max-w-[240px] truncate font-mono" title={m}>
                                  {m}
                                </span>
                                <button
                                  type="button"
                                  className="rounded-full p-0.5 text-zinc-500 hover:bg-zinc-200"
                                  aria-label={`Remove model ${m}`}
                                  onClick={() => removeModelFromNewProvider(m)}
                                >
                                  <X className="h-3 w-3" />
                                </button>
                              </div>
                            ))
                          ) : (
                            <div className="text-xs text-zinc-500">No models</div>
                          )}
                        </div>
                        <div className="mt-2 flex gap-2">
                          <Input
                            className="font-mono text-xs"
                            placeholder="Add model"
                            value={newProviderModelDraft}
                            onChange={(e) => setNewProviderModelDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                e.preventDefault();
                                addModelsToNewProvider(newProviderModelDraft);
                              }
                            }}
                          />
                          <Button
                            variant="outline"
                            disabled={settingsSaving || !newProviderModelDraft.trim()}
                            onClick={() => addModelsToNewProvider(newProviderModelDraft)}
                          >
                            Add
                          </Button>
                        </div>
                      </div>

                      <div>
                        <div className="text-xs text-zinc-600">API key (optional)</div>
                        <Input
                          type="password"
                          placeholder="sk-…"
                          value={newProviderApiKey}
                          onChange={(e) => setNewProviderApiKey(e.target.value)}
                        />
                      </div>

                      <div className="flex justify-end">
                        <Button onClick={addProvider} disabled={settingsSaving}>
                          Add provider
                        </Button>
                      </div>
                    </div>
                  ) : providersPaneSelectedProvider ? (
                    <div className="space-y-4">
                      <div>
                        <div className="text-xs text-zinc-600">Base URL</div>
                        <Input
                          value={providersPaneSelectedProvider.baseUrl}
                          onChange={(e) =>
                            setProviderDraft(providersPaneSelectedProvider.id, {
                              baseUrl: e.target.value,
                            })
                          }
                        />
                      </div>

                      <div>
                        <div className="flex items-center justify-between">
                          <div className="text-xs text-zinc-600">Models</div>
                          <div className="text-xs text-zinc-500">
                            {providersPaneSelectedProvider.models.length} total
                          </div>
                        </div>
                        <div className="mt-2 flex flex-wrap gap-2">
                          {providersPaneSelectedProvider.models.length ? (
                            providersPaneSelectedProvider.models.map((m) => (
                              <div
                                key={m}
                                className="flex min-w-0 max-w-full items-center gap-1 rounded-full border border-zinc-200 bg-zinc-50 px-2 py-1 text-xs"
                              >
                                <span className="max-w-[240px] truncate font-mono" title={m}>
                                  {m}
                                </span>
                                <button
                                  type="button"
                                  className="rounded-full p-0.5 text-zinc-500 hover:bg-zinc-200"
                                  aria-label={`Remove model ${m}`}
                                  onClick={() => removeModelFromProvider(providersPaneSelectedProvider.id, m)}
                                >
                                  <X className="h-3 w-3" />
                                </button>
                              </div>
                            ))
                          ) : (
                            <div className="text-xs text-zinc-500">No models</div>
                          )}
                        </div>
                        <div className="mt-2 flex gap-2">
                          <Input
                            className="font-mono text-xs"
                            placeholder="Add model"
                            value={providersPaneSelectedProvider.modelDraft}
                            onChange={(e) =>
                              setProviderDraft(providersPaneSelectedProvider.id, {
                                modelDraft: e.target.value,
                              })
                            }
                            onKeyDown={(e) => {
                              if (e.key === "Enter") {
                                e.preventDefault();
                                addModelsToProvider(
                                  providersPaneSelectedProvider.id,
                                  providersPaneSelectedProvider.modelDraft,
                                );
                              }
                            }}
                          />
                          <Button
                            variant="outline"
                            disabled={settingsSaving || !providersPaneSelectedProvider.modelDraft.trim()}
                            onClick={() =>
                              addModelsToProvider(
                                providersPaneSelectedProvider.id,
                                providersPaneSelectedProvider.modelDraft,
                              )
                            }
                          >
                            Add
                          </Button>
                        </div>
                      </div>

                      <div>
                        <div className="flex items-center justify-between">
                          <div className="text-xs text-zinc-600">API key</div>
                          <div className="text-xs text-zinc-500">
                            {providersPaneSelectedProvider.apiKeySet ? "set" : "not set"}
                          </div>
                        </div>
                        <div className="mt-2 flex items-end gap-2">
                          <div className="min-w-0 flex-1">
                            <Input
                              type="password"
                              placeholder="Enter new API key"
                              value={providersPaneSelectedProvider.apiKeyDraft}
                              onChange={(e) =>
                                setProviderDraft(providersPaneSelectedProvider.id, {
                                  apiKeyDraft: e.target.value,
                                })
                              }
                            />
                          </div>
                          <Button
                            onClick={() => updateProviderApiKey(providersPaneSelectedProvider.id)}
                            disabled={
                              settingsSaving || !providersPaneSelectedProvider.apiKeyDraft.trim()
                            }
                          >
                            Update
                          </Button>
                          <Button
                            variant="outline"
                            onClick={() => clearProviderApiKey(providersPaneSelectedProvider.id)}
                            disabled={settingsSaving || !providersPaneSelectedProvider.apiKeySet}
                          >
                            Clear
                          </Button>
                        </div>
                        <div className="mt-1 text-xs text-zinc-500">
                          Keys are stored in <span className="font-mono">kiliax.yaml</span> and are
                          not shown again.
                        </div>
                      </div>

                      <div className="flex items-center justify-between">
                        <Button
                          variant="outline"
                          disabled={settingsSaving}
                          onClick={() => loadSettingsProviders().catch(onApiError)}
                        >
                          Revert
                        </Button>
                        <div className="flex gap-2">
                          <Button
                            variant="outline"
                            className="border-rose-200 text-rose-700 hover:bg-rose-50"
                            disabled={settingsSaving}
                            onClick={() =>
                              setProviderDeleteConfirm({
                                providerId: providersPaneSelectedProvider.id,
                              })
                            }
                          >
                            Delete
                          </Button>
                          <Button
                            disabled={settingsSaving}
                            onClick={() => saveProvider(providersPaneSelectedProvider.id)}
                          >
                            Save
                          </Button>
                        </div>
                      </div>
                    </div>
                  ) : (
                    <div className="py-10 text-center text-sm text-zinc-500">Select a provider</div>
                  )}
                </div>
              </div>

              <datalist id="kiliax-model-suggestions">
                {defaultModelSuggestions.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
            </div>
          ) : settingsTab === "agents" ? (
            <div className="space-y-3">
              <div className="rounded-md border border-zinc-200 bg-white p-3">
                <div className="text-sm font-medium text-zinc-900">Max steps</div>
                <div className="mt-2 grid grid-cols-1 gap-2 sm:grid-cols-3">
                  <div>
                    <div className="text-xs text-zinc-600">runtime.max_steps</div>
                    <Input
                      placeholder="default: 1024"
                      value={runtimeMaxSteps}
                      onChange={(e) => setRuntimeMaxSteps(e.target.value)}
                    />
                  </div>
                  <div>
                    <div className="text-xs text-zinc-600">agents.plan.max_steps</div>
                    <Input
                      placeholder="optional"
                      value={agentsPlanMaxSteps}
                      onChange={(e) => setAgentsPlanMaxSteps(e.target.value)}
                    />
                  </div>
                  <div>
                    <div className="text-xs text-zinc-600">agents.general.max_steps</div>
                    <Input
                      placeholder="optional"
                      value={agentsGeneralMaxSteps}
                      onChange={(e) => setAgentsGeneralMaxSteps(e.target.value)}
                    />
                  </div>
                </div>
                <div className="mt-2 text-xs text-zinc-500">Leave blank to use defaults.</div>
                <div className="mt-3 flex justify-end gap-2">
                  <Button
                    variant="outline"
                    disabled={settingsSaving}
                    onClick={() => loadSettingsRuntime().catch(onApiError)}
                  >
                    Reload
                  </Button>
                  <Button onClick={saveRuntimeConfig} disabled={settingsSaving}>
                    Save
                  </Button>
                </div>
              </div>
            </div>
          ) : (
            <div className="space-y-2">
              <div className="text-xs text-zinc-600">
                Path: <span className="font-mono">{configPath || "kiliax.yaml"}</span>
              </div>
              <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
                Warning: raw config may include secrets.
              </div>
              <Textarea
                className="h-[420px] font-mono text-xs"
                value={configYaml}
                onChange={(e) => setConfigYaml(e.target.value)}
              />
              <div className="flex justify-end gap-2">
                <Button
                  variant="outline"
                  onClick={() => ensureConfigLoaded(true).catch(onApiError)}
                  disabled={savingConfig}
                >
                  Reload
                </Button>
                <Button onClick={saveConfig} disabled={savingConfig || !configLoaded}>
                  {savingConfig ? "Saving…" : "Save"}
                </Button>
              </div>
            </div>
          )}
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(providerDeleteConfirm)} onOpenChange={(o) => !o && setProviderDeleteConfirm(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete provider?</DialogTitle>
            <DialogDescription className="truncate">{providerDeleteConfirm?.providerId ?? ""}</DialogDescription>
          </DialogHeader>
          <div className="mt-3 text-sm text-zinc-600">
            This updates global config and may affect sessions using this provider.
          </div>
          <div className="mt-3 flex justify-end gap-2">
            <Button variant="outline" onClick={() => setProviderDeleteConfirm(null)}>
              Cancel
            </Button>
            <Button
              className="bg-red-600 text-zinc-50 hover:bg-red-500"
              disabled={settingsSaving}
              onClick={async () => {
                const id = providerDeleteConfirm?.providerId;
                if (!id) return;
                setProviderDeleteConfirm(null);
                await deleteProvider(id);
              }}
            >
              Delete
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
