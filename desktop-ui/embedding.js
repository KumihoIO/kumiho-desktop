(function exposeEmbedding(root) {
  const PRESETS = [
    {
      label: "OpenAI — text-embedding-3-small (1536)",
      provider: "openai",
      model: "text-embedding-3-small",
      dimensions: 1536,
      endpoint: "",
      send_dimensions: true,
      batch_size: 20,
    },
    {
      label: "OpenAI — text-embedding-3-large (3072)",
      provider: "openai",
      model: "text-embedding-3-large",
      dimensions: 3072,
      endpoint: "",
      send_dimensions: true,
      batch_size: 20,
    },
    {
      label: "Cloudflare — bge-m3 (1024)",
      provider: "openai",
      model: "bge-m3",
      dimensions: 1024,
      endpoint: "https://api.cloudflare.com/client/v4/accounts/<account_id>/ai/run/@cf/baai/bge-m3",
      send_dimensions: false,
      batch_size: 20,
    },
    {
      label: "Self-hosted — BGE-M3 via TEI / vLLM (1024)",
      provider: "openai",
      model: "BAAI/bge-m3",
      dimensions: 1024,
      endpoint: "http://localhost:8080/v1/embeddings",
      send_dimensions: false,
      batch_size: 20,
    },
    {
      label: "Custom OpenAI-compatible",
      provider: "openai",
      model: "",
      dimensions: 1536,
      endpoint: "",
      send_dimensions: null,
      batch_size: 20,
    },
  ];

  function validateEmbeddingConfig(cfg) {
    if (!cfg.enabled) return "";
    if (!cfg.api_key || !cfg.api_key.trim()) return "Embedding API key is required when vector search is enabled.";
    if (!cfg.model || !cfg.model.trim()) return "Embedding model is required.";
    const dims = Number(cfg.dimensions);
    if (!Number.isInteger(dims) || dims < 64 || dims > 4096) return "Dimensions must be an integer between 64 and 4096.";
    const batch = Number(cfg.batch_size);
    if (!Number.isInteger(batch) || batch < 1 || batch > 100) return "Batch size must be between 1 and 100.";
    if (cfg.endpoint && cfg.endpoint.trim()) {
      try {
        const u = new URL(cfg.endpoint.trim());
        if (u.protocol !== "http:" && u.protocol !== "https:") return "Endpoint must be http(s)://";
      } catch (_) {
        return "Endpoint is not a valid URL.";
      }
    }
    return "";
  }

  function presetForModel(model) {
    const m = (model || "").toLowerCase();
    if (m.includes("bge-m3") || m.includes("bge")) return PRESETS[2];
    if (m.includes("3-large")) return PRESETS[1];
    return PRESETS[0];
  }

  function applyPresetToFields(preset, fields) {
    if (!preset || !fields) return;
    if (fields.provider) fields.provider.value = preset.provider;
    if (fields.model) fields.model.value = preset.model;
    if (fields.dimensions) fields.dimensions.value = String(preset.dimensions);
    if (fields.endpoint) fields.endpoint.value = preset.endpoint;
    if (fields.sendDimensions && preset.send_dimensions !== null) {
      fields.sendDimensions.value = preset.send_dimensions ? "true" : "false";
    }
    if (fields.batchSize) fields.batchSize.value = String(preset.batch_size);
  }

  const api = {
    PRESETS,
    validateEmbeddingConfig,
    presetForModel,
    applyPresetToFields,
  };
  root.KumihoDesktopEmbedding = api;
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
})(typeof globalThis !== "undefined" ? globalThis : this);
