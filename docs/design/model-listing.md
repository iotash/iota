# Listing image models across providers: one reading rule, bounded special cases

Status: **Design** · 2026-11-04 · `listing-design` on `42e1360` (v0.7.5) · the in-flight `images-fix` = `c9e468a` (not on main) · measurements of 2026-11-04 in DIVERGENCES X-86 · survey read 2026-11-04 from the vendors' current reference pages and the tools' sources

The owner's question: `type=image` is a Venice-specific query. Other platforms say which models draw in other ways, so how does the `images` listing stay generic without growing a case per vendor? No code changes here.

**Short answer.** Keep the mechanism `c9e468a` landed, but name what it is and bound it: the page itself says which *vocabulary* it speaks (`type`, `output_modalities`, or nothing), and a vocabulary — never a vendor — owns one filter and at most one server-side query. The query is asked whenever the page speaks a vocabulary that owns one (two vendors document that their bare list is filtered to text by default, and OpenRouter's bare page still shows a few text+image models, so "the page admitted something" proves nothing), never on a page that speaks none. "N models listed, none marked as an image model" becomes a picker note instead of a silent empty list. Everything lives in `src/llm/images.rs`; the `Provider` trait, the catalog and the config stay as they are. A vendor earns a row only by a documented, measured response signal; a vendor that offers nothing (OpenAI, xAI's `/models`) lists everything and the user judges, as the Go original and the OpenAI SDK do.

## 1. What the platforms and the tools actually expose

| platform | how image models are found | signal | exact field / parameter | source |
|---|---|---|---|---|
| Venice | `GET /models` lists text only ("if omitted the response includes only text models"); `?type=image` lists the image models; every entry carries `type` (asr, decision, embedding, image, music, text, tts, upscale, inpaint, video; `all`) | query + field | `type` / `?type=` | docs.venice.ai/api-reference/endpoint/models/list; measured 139 / 43 |
| OpenRouter | `GET /api/v1/models?output_modalities=` (text, image, embeddings, audio, video, …) **defaults to `text`**; entries carry `architecture.output_modalities`; `GET /api/v1/images/models` is an image-only list. Live: bare list 455 of which 12 output image (text+image models); `?output_modalities=image` → 63 | query + field + separate endpoint | `architecture.output_modalities` / `?output_modalities=` | openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties; live probe |
| xAI | `GET /v1/models` ignores `?type=image`, entries have no modality; per-kind `GET /v1/language-models`, `/v1/image-generation-models`, `/v1/video-generation-models`, whose entries carry `output_modalities` | separate endpoint + field; nothing on `/models` | `/v1/image-generation-models`; `output_modalities` | docs.x.ai/developers/rest-api-reference/inference/models; measured |
| OpenAI | `GET /models` ignores `?type=image`; entries are `id, object, created, owned_by`; the SDKs' `models.list()` takes no filter | nothing | — | developers.openai.com/docs/api-reference/models/list; openai-python `types/model.py`; measured |
| Together | `GET /v1/models` entries carry `type` (chat, language, code, image, embedding, moderation, rerank, audio, transcribe, video — enum from the official SDK; the docs page serves only the catalog); no query. `GET /v2/supported-models`: `?modality=` (input side), entries `outputModalities`, `capabilities` incl. `CAPABILITY_IMAGE_GENERATION` | field (v1); query + field (v2) | `type`; `outputModalities` | together-python `types/models.py`; docs.together.ai/reference/dmi/supported-models-list |
| Fireworks | `GET /v1/accounts/{id}/models` entries carry `kind` (packaging: HF_BASE_MODEL, FLUMINA_BASE_MODEL, …), `supportsImageInput`, `supportsTools`; no output flag; `?filter=` is AIP-160 | nothing usable | `kind` (indirect) | docs.fireworks.ai/api-reference/list-models |
| Gemini | `GET /v1beta/models` entries carry `supportedGenerationMethods` (string list, no enum); the `predict` value `imagen.rs` keys on is not on the current page (Imagen documented as shut down) | field (method list) | `supportedGenerationMethods` | aistudio.google.com/api-reference/models |
| DeepInfra | `GET /models/list` (its own path) entries carry `reported_type`; live 398: text-generation 230, text-to-image 55, text-to-video 29, … | field, other spelling | `reported_type: text-to-image` | docs.deepinfra.com/api-reference/models/models-list |
| Replicate | model objects have no modality field; discovery is `GET /v1/collections/text-to-image`, `QUERY /v1/models` full text, or a version's `openapi_schema` output type | separate endpoint | collection slug `text-to-image` | replicate.com/docs/reference/http |
| fal.ai | `GET api.fal.ai/v1/models?category=text-to-image`; entries carry `metadata.category` | query + field, other spelling | `category` | fal.ai/docs/platform-apis/v1/models |
| Stability | v2beta has no listing: the model is the path (`/v2beta/stable-image/generate/{core,ultra,sd3}`) plus a body `model`; legacy `GET /v1/engines/list` carried `type: PICTURE` | nothing (v2) / field (v1) | `type: PICTURE` | docs.stability.ai/api-reference |
| Hugging Face | Hub `GET /api/models?pipeline_tag=text-to-image`; the router's `GET /v1/models` lists chat models only and copies OpenRouter's `architecture.*_modalities` | query (hub) / field (router) | `pipeline_tag`; `architecture.output_modalities` | huggingface.co/docs/inference-providers/hub-api |
| Azure OpenAI, Mistral | `capabilities` objects (chat, embeddings, vision, fine-tune, …) with no image-generation flag | nothing | `capabilities` | learn.microsoft.com/rest/api/azureopenai/models/list; docs.mistral.ai/api/endpoint/models |

| tool | how it knows a model makes images | signal | exact field / parameter | source |
|---|---|---|---|---|
| LiteLLM | static `model_prices_and_context_window.json`: `mode: "image_generation"` (412 entries; also `image_edit`), `supported_endpoints`, a few `supported_output_modalities`; the proxy's `GET /v1/models` has no mode filter ("use `/model/info`"); a live id probe is opt-in (`check_provider_endpoint`, default off) and returns ids only — capability still comes from the table | static table | `mode`; `/model/info` | github BerriAI/litellm `model_prices_and_context_window.json`, `utils.py::get_valid_models`, `proxy_server.py` |
| models.dev → opencode, Cline | catalog entries carry `type` (176 `image`) and `modalities.output`; `api.json?type=image` works. opencode fetches `?type=chat,decision,image` and lists non-chat models only when asked by type; Cline derives `image-generation` from `modalities.output` and then hides those models | static catalog + query | `type`, `modalities.output`, `?type=` | models.dev/api.json; anomalyco/opencode `core/src/models-dev.ts`, `model.ts`; cline `sdk/packages/llms/src/catalog/model-filters.ts` |
| Open WebUI | chat list = `GET /models` per connection, merged; only on `api.openai.com` an id denylist (`dall-e`, `tts`, `whisper`, …); image generation is a separate engine whose OpenAI/Gemini model lists are **hardcoded** (`dall-e-3`, `gpt-image-1`, `imagen-3.0-generate-002`) | denylist / hardcoded list | `IMAGE_GENERATION_ENGINE` | open-webui `routers/openai.py`, `routers/images.py` |
| aider | LiteLLM's table; keeps `mode == "chat"` only, never shows an image model | static table | `mode` | Aider-AI/aider `aider/models.py` |
| Vercel AI SDK | no listing API; the caller constructs `provider.image(id)` (`ImageModelV4`) | none, user picks | — | ai-sdk.dev/docs/ai-sdk-core/image-generation |
| Codex CLI | `GET /models?client_version=` plus its own catalog (`model_providers.<id>.model_catalog_url`, a document in Codex's shape, not the vendor's); no image models | id listing + own catalog | `model_catalog_url` | openai/codex `codex-api/src/endpoint/models.rs` |
| Claude Code, Gemini CLI, Goose, Continue | the user names the model (`/model`, `NANOBANANA_MODEL`, `GOOSE_MODEL`, `roles:`); no image-model listing | none, user picks | — | code.claude.com/docs/en/model-config; gemini-cli-extensions/nanobanana; goose-docs.ai; docs.continue.dev/reference |

The shape of the data: the signal is a **per-entry field** on nine listings and a **query** on five (Venice, OpenRouter, fal, the HF hub, models.dev); three use a **separate endpoint** (xAI, OpenRouter's `/images/models`, Replicate's collections); OpenAI, xAI's `/models`, Azure, Mistral, Fireworks and Stability v2 give nothing. The names that recur are vocabularies shared across vendors, not vendor names: `type` (Together, Venice, Stability v1, DeepInfra, models.dev) and `output_modalities` (OpenRouter, xAI, the HF router, Together v2). The trap to name: Venice and OpenRouter both **default their bare list to text**, so a plain `GET /models` silently omits image models — and OpenRouter's bare page still shows the twelve text+image models, so a page that admits something is not thereby whole. Cross-provider tools never probe: LiteLLM, aider, opencode and Cline ship a static catalog and read a live `/models` as ids only; every default leans to text (OpenRouter, opencode, aider, Cline); and when nothing is known they list everything or let the user name the model — Open WebUI and the nanobanana extension hardcode their image lists; no tool shows an empty picker on purpose.

## 2. What iota has today

- `Images::image_models` (`src/llm/images.rs`) fetches `/models`, keeps an entry when `type == "image"`, or when `type` is absent and the top-level `output_modalities` is empty or names `image`; when a non-empty page admits nothing it fetches the fixed `/models?type=image` once and filters the same way. `ImagesProvider::list_models` (`src/provider/images.rs`) maps ids; the `Provider` trait is untouched.
- Two precedents already encode the shape: `imagen.rs::image_capable` keeps a model by `supportedGenerationMethods: predict` (official form) or `outputModalities: image` (relay form), entries without metadata kept; `google.rs::models` tries ONE fallback path (`/v1beta/models`), only on Vertex and only when the error class says so, and returns the original error when the fallback fails.
- The picker (X-26, `src/repl/catalog.rs`) already treats a failed source as "zero rows plus one note in the prompt row" and keeps the input row open; an `Ok(empty)` from a source is silent — no row, no note. The written-down half of the catalog (`models:` entries, inline `provider:id`) never depends on a listing at all.
- Config: `providers.<name>` accepts exactly `type`, `key`, `url` (`config/strict.rs`); `json_edits` is a `models.<name>` key, a property of the model, not of the endpoint.

## 3. Options

- **A. Probe chain.** Page admits nothing → try `?type=image` → try the next known query. Lives in the dialect; bounded by the list of queries. Each probe costs a request on every endpoint that lists nothing, a strict validator can answer 400 to a parameter it does not know (Venice validates its bodies that way), and when every probe fails the user sees what the last one said: nothing.
- **B. Listing dialect per vendor.** A `ListingDialect` enum or a trait on the provider, picked by `type:` or by URL. Testable, but the `images` type is *defined* as "OpenAI-shaped"; a `venice` kind is a class per vendor, and a URL sniff is a name heuristic in disguise. A dialect keyed on the *response shape* rather than the vendor is the vocabulary table of §4 — B done right collapses into it.
- **C. Config key on `providers.<name>`** (`models: {path, query, keep}`). Zero code per vendor, but it is the first non-credential endpoint key, the user must know the vendor's listing API to write it, every field needs validation and docs, and it buys nothing the written-down half of the catalog does not already give: a user who knows the model id writes a `models:` entry. Codex's `model_catalog_url` is the nearest precedent, and it points at a document in Codex's own shape, not at the vendor's listing.
- **D. Metadata only, no query.** Honest and cheap, but Venice's bare page is 139 text models and OpenRouter's hides 51 of 63, so an `images` agent on either opens an empty or a truncated picker for image models that are one documented query away.
- **E. The seams that exist.** The dialect's `models()` with a signal-triggered fallback (google), the metadata filter with an "unknown is kept" default (imagen), and the catalog's note-per-failed-source (X-26). Nothing new has to be built for the shape; what is missing is the *rule* that bounds it and the note.

| option | generic vs special | code cost | config cost | failure mode | testability |
|---|---|---|---|---|---|
| A chain | special, grows per query | one branch per probe | none | silent empty after N requests; 400 on strict validators | wiremock per probe, order-dependent |
| B per-vendor dialect | special by construction | one impl per vendor, a `kind` or URL match | a new `type:` value per vendor | wrong dialect on a relay that mirrors a vendor's URL | good per dialect, poor for relays |
| C config key | generic in theory, special in every user's file | parser + validator + docs | high: the user must know the vendor API | typo = error at load; stale when the vendor changes | config tests only; the network is still asked |
| D metadata only | fully generic | removes 10 lines | none | empty picker on Venice, 12 of 63 on OpenRouter | trivial |
| E/§4 vocabulary table | generic rule, special rows | ≈ today's code plus one error variant | none | note names the count; typed id and `models:` rows stand | one wiremock page per row; request count asserted |

## 4. Recommendation

**The layer.** `src/llm/images.rs`, and only there. `Images::image_models` stays the one function that knows how a listing *reads*; `ImagesModel::makes_images` becomes a `Vocabulary` read off the page, with that vocabulary's `admits(entry)` and its optional `query`. `ImagesProvider::list_models` (`src/provider/images.rs`) stays the thin map to ids and gains one error mapping. `Provider::list_models` keeps its `Vec<String>` signature; `src/repl/catalog.rs` and the picker change nothing — the note path they already have is what the new outcome rides. Dialect-local, like `google.rs::models` and `imagen.rs::image_capable`.

**The rule**, in the order the code runs it:

1. One `GET /models`. The page names its vocabulary: `type` when any entry carries a string `type`; else `output_modalities` when any entry carries `output_modalities` or `architecture.output_modalities` (OpenRouter's placement, copied by the HF router and relays); else *none*.
2. Keep by that vocabulary: `type == "image"`; `output_modalities` names `image`, an entry whose list is empty kept (the user judges, as today); *none* keeps every entry.
3. A server-side query belongs to the vocabulary, not to a vendor, and each of the two owns exactly one: `type` → `?type=image` (Venice's), `output_modalities` → `?output_modalities=image` (OpenRouter's). It is asked once whenever the page spoke that vocabulary — both vendors document a text-only default, and OpenRouter's bare page admits twelve models while hiding fifty-one, so "admitted something" cannot be the trigger. Its answer is filtered by the same rule and unioned with the bare page's rows, deduplicated by id.
4. An endpoint that ignores the query answers the same page (Together: one wasted request, the same rows). One that refuses it costs nothing the bare page already gave: the bare rows stand, and only with none is the query's error the listing's error.
5. A page with no vocabulary never sees a query (OpenAI, xAI). Budget: at most two requests per listing, under the catalog's cancel scope (X-26).

This is not a probe: the first page already said "I label models by `type` and none of these is `image`", which is exactly the sentence the documented query answers.

**The outcome when nothing is admitted.** Not `Ok(vec![])`: a `ProviderError` variant whose first line is `139 models listed, none marked as an image model — type a model id, or write a models: entry`. X-26 already turns a source's error into one note in the picker's prompt row (`venice: 139 models listed, …`), keeps every written-down row, and leaves the input row open; `-M provider:id` and `models:` entries never touch the listing. An *empty* page (zero entries) stays `Ok(empty)` and silent — the endpoint said nothing. Nothing is printed over the transcript, no block, no ladder. The same note serves a relay that copies a field but ignores the query: two requests, one honest sentence.

**When a vendor earns a row** — all four, or none:

- (a) A dated live probe (the X-86 method: both pages fetched, counted, written into DIVERGENCES) shows the generic reading listing non-image models beside image ones, or none, while the vendor serves image models on that endpoint.
- (b) The vendor documents the signal: a field on the entry, or a query on the same endpoint paired with such a field.
- (c) It is expressible as a row — field name, the values that mean image, optional query — never the vendor's name, its URL, or a model-id pattern.
- (d) One wiremock test shaped like the real page, and a DIVERGENCES line with the numbers.

Applied today:

- Venice is the `type` row (in). Together v1 rides it for free: its bare page already says `type: image`.
- OpenRouter and the relays that copy its `architecture` block are the `output_modalities` row, whose query is the one addition `c9e468a` lacks. Whether OpenRouter's own image endpoint takes the OpenAI body is unverified and separate from the listing.
- OpenAI and xAI earn nothing and list everything — the Go original's behaviour and the OpenAI SDK's. xAI's `/v1/image-generation-models` is a *path* signal, which no bare page can trigger: a path probe needs a vendor identity, which is the case this rule refuses. The day a second vendor needs a path, `path` becomes a row attribute chosen by `type:` — not before.
- Other spellings of "image" (`PICTURE`, `text-to-image`, `MODALITY_IMAGE`) live on APIs that are not OpenAI-shaped `/models` pages and earn nothing until one appears on such a page.

## 5. Tests and verification

- **Unit, `llm::images`:** the vocabulary read off the three page shapes; a mixed page (some entries with `type`) reads as `type`; `type: null` or a number reads as absent; `architecture.output_modalities` reads as `output_modalities`; the union deduplicates by id and keeps the sort.
- **Wiremock, `tests/provider/images.rs`:** keep `a_page_typed_for_several_kinds_lists_only_the_type_image_entries`, `a_venice_shaped_page_of_text_models_asks_for_the_image_ones` (two requests, the second `type=image`) and `an_openai_shaped_page_keeps_every_entry_without_a_type` (one request). Flip `an_endpoint_that_ignores_the_query_lists_nothing_rather_than_text_models`: the second request is now `output_modalities=image` and the result is the none-marked error carrying the count, not `Ok(empty)`. Add: an OpenRouter-shaped page whose bare list admits two and whose query answers five lists seven; a Together-shaped page (`type: chat|image|embedding`) lists its image entries, the ignored query adding none; a strict endpoint answering 400 to the query keeps the bare rows, and with no bare rows reports that 400; an empty page is `Ok(empty)` in one request.
- **Catalog, `src/repl/catalog.rs` tests:** a wildcard source answering the none-marked error contributes zero rows and the note `venice: 139 models listed, …` while the `models:` rows and the typed row stand (the X-26 contract: unchanged code, new input).
- **Live, dated, before merge** (the provider-probe method: scratch config, requests recorded): Venice (two requests, 43 rows), OpenAI (one request, every model), xAI (one request, every model), OpenRouter (two requests, 63 rows), Together if a key is at hand. The numbers replace the X-86 line's.
- **Control:** the Venice case against `4742e11` lists the 139 text rows; against `c9e468a` the OpenRouter case lists 12 and the ignored-query case is a silent empty picker; on the branch they are 63 and the note.

## 6. What not to do

- No name heuristics (`dall-e`, `flux`, `sd`), no URL sniff (`venice.ai`, `openrouter.ai`), no `ProviderKind::Venice`: the Go original's rule, "the user judges, never name heuristics", stands.
- No unbounded chain, no query on a page that speaks no vocabulary, no query on the first request: a parameter an endpoint does not know can be a 400 on a strict validator.
- No static catalog bundled in the binary (LiteLLM's `mode` table, models.dev): iota's rule is "the endpoint says", and a table ages by the week — the tools that ship one also ship the lag (models.dev's README still documents one `type` value while its data carries seven).
- No body prose read to decide anything (X-84); a 400 on the query is reported, not interpreted.
- No listing key under `providers.<name>` until a vendor shows a signal no row can express, and even then the `models:` entry is the cheaper escape hatch the user already has.
- Do not widen `Provider::list_models` to return metadata for every dialect to serve one dialect; do not make "none marked" a failure that hides the picker or prints over the transcript; do not make an empty page an error.
