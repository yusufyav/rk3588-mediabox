//! One title: what it is, how it can be played, and what that will cost.
//!
//! The source list is the heart of the product. Every technical claim on this
//! screen comes from the media core's own inspection and policy output — the
//! probe that opened the file and the decision made against this board's
//! accepted capability profile. Nothing is inferred from a release name: a
//! source called "2160p HDR DV" that probes as 8-bit SDR is drawn as 8-bit SDR.
//!
//! Where the appliance cannot do something, it says so rather than degrading
//! quietly. A Dolby Vision source is never presented as safe HDR10, a torrent
//! whose pieces never arrive is shown as unreachable rather than played into
//! silence, and preview is offered only when the browser can genuinely open
//! the source.

use crate::app::{Nav, Recent, Route, Toaster, remember};
use crate::components::{
    Action, Chip, Failure, Load, human_bitrate, human_size, resolution_label, seconds_to_clock,
};
use crate::model::{
    LibraryItemEnvelope, Meta, MetaEnvelope, Plan, Reason, Stream, StreamListing, VideoTrack,
};
use crate::{api, LIBRARY_ADDON_ID};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

/// A stream as the UI holds it: the parsed view for drawing, and the exact
/// descriptor the media core handed over, which is what must be sent back.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    parsed: Stream,
    raw: Value,
}

#[component]
pub fn Detail(kind: String, id: String) -> impl IntoView {
    let nav = expect_context::<Nav>();
    let toaster = expect_context::<Toaster>();

    let meta = RwSignal::new(Load::Loading);
    let sources = RwSignal::new(Load::<Vec<Source>>::Loading);
    let selected = RwSignal::new(None::<Source>);
    let plan = RwSignal::new(None::<Load<Plan>>);
    let preview_url = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let is_library = kind == "library" || id.starts_with("library:");
    let load_kind = kind.clone();
    let load_id = id.clone();

    spawn_local(async move {
        // A library title is answered in one call, meta and sources together;
        // an addon title needs the two separate reads.
        if is_library {
            match api::typed::<LibraryItemEnvelope>(api::media_library_item(&load_id)).await {
                Ok(found) => {
                    meta.set(Load::Ready(found.meta));
                    sources.set(Load::Ready(to_sources(found.streams)));
                }
                Err(error) => {
                    meta.set(Load::Failed(error.message.clone()));
                    sources.set(Load::Failed(error.message));
                }
            }
            return;
        }
        match api::typed::<MetaEnvelope>(api::media_meta(&load_kind, &load_id)).await {
            Ok(found) => meta.set(Load::Ready(found.meta)),
            Err(error) => meta.set(Load::Failed(error.message)),
        }
        match api::control(api::media_streams(&load_kind, &load_id)).await {
            Ok(value) => {
                let listing: StreamListing =
                    serde_json::from_value(value.clone()).unwrap_or(StreamListing {
                        streams: Vec::new(),
                        playable: 0,
                    });
                let raw = value
                    .get("streams")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let found: Vec<Source> = listing
                    .streams
                    .into_iter()
                    .zip(raw)
                    .map(|(parsed, raw)| Source { parsed, raw })
                    .collect();
                sources.set(Load::Ready(found));
            }
            Err(error) => sources.set(Load::Failed(error.message)),
        }
    });

    let analyze = move |source: Source| {
        selected.set(Some(source.clone()));
        if !source.parsed.playable {
            plan.set(None);
            return;
        }
        plan.set(Some(Load::Loading));
        spawn_local(async move {
            let request = match source.parsed.url.as_deref() {
                Some(url) => api::media_policy(url),
                None => api::media_stream_plan(&source.raw),
            };
            match api::typed::<Plan>(request).await {
                Ok(found) => plan.set(Some(Load::Ready(found))),
                Err(error) => plan.set(Some(Load::Failed(error.message))),
            }
        });
    };

    // The first source that can be played is the one worth analysing without
    // being asked; everything else stays a deliberate choice.
    Effect::new(move |_| {
        if selected.get_untracked().is_some() {
            return;
        }
        if let Load::Ready(found) = sources.get()
            && let Some(first) = found.iter().find(|source| source.parsed.playable).cloned()
        {
            analyze(first);
        }
    });

    let remember_played = move || {
        if let Load::Ready(found) = meta.get_untracked() {
            remember(Recent {
                id: found.id.clone(),
                kind: found.kind.clone(),
                name: found.name.clone(),
                poster: found.poster.clone(),
                background: found.background.clone(),
                release_info: found.release_info.clone(),
            });
        }
    };

    let play = Callback::new(move |()| {
        let Some(source) = selected.get_untracked() else {
            toaster.warn("Önce bir kaynak seçin.");
            return;
        };
        busy.set(true);
        toaster.say("Televizyona gönderiliyor…");
        spawn_local(async move {
            let request = match source.parsed.url.as_deref() {
                Some(url) => api::play_on_kodi(Some(url), None, 0),
                None => api::play_on_kodi(None, Some(&source.raw), 0),
            };
            match api::control(request).await {
                Ok(_) => {
                    remember_played();
                    toaster.say("Kodi'de oynatılıyor.");
                    nav.go(Route::NowPlaying);
                }
                Err(error) => toaster.warn(format!("Oynatılamadı — {}", error.message)),
            }
            busy.set(false);
        });
    });

    let preview = Callback::new(move |()| {
        let Some(source) = selected.get_untracked() else {
            return;
        };
        let Some(Load::Ready(found)) = plan.get_untracked() else {
            return;
        };
        match previewable_url(&found, &source) {
            Some(url) => {
                remember_played();
                preview_url.set(Some(url));
            }
            None => toaster.warn(
                "Bu kaynak tarayıcıda önizlenemiyor; televizyonda oynatılabilir.",
            ),
        }
    });

    view! {
        <div class="detail">
            {move || match meta.get() {
                Load::Loading => {
                    view! { <div class="state"><strong>"Yükleniyor…"</strong></div> }.into_any()
                }
                Load::Failed(detail) => {
                    view! { <Failure title="Başlık açılamadı" detail=detail /> }.into_any()
                }
                Load::Ready(found) => {
                    view! {
                        <Head
                            meta=found
                            plan=plan
                            selected=selected
                            busy=busy
                            on_play=play
                            on_preview=preview
                        />
                    }
                        .into_any()
                }
            }}
            <SourceList sources=sources selected=selected on_pick=Callback::new(analyze) />
            {move || {
                plan.get()
                    .map(|state| view! { <Technical state=state /> })
            }}
            {move || {
                preview_url
                    .get()
                    .map(|url| {
                        view! {
                            <PreviewSheet
                                url=url
                                on_close=Callback::new(move |()| preview_url.set(None))
                            />
                        }
                    })
            }}
        </div>
    }
}

fn to_sources(streams: Vec<Stream>) -> Vec<Source> {
    streams
        .into_iter()
        .map(|parsed| {
            let raw = serde_json::to_value(&parsed.url).unwrap_or(Value::Null);
            Source {
                raw: serde_json::json!({"url": raw, "addonId": LIBRARY_ADDON_ID}),
                parsed,
            }
        })
        .collect()
}

/// Whether the browser can genuinely open this source, and at what URL.
///
/// Only `BrowserDirect` qualifies, and only when the resolved URL is one a
/// browser can actually fetch. A loopback URL is reachable from the television
/// but not from a phone, and a preview that plays on one device and silently
/// fails on another is worse than an honest refusal.
fn previewable_url(plan: &Plan, source: &Source) -> Option<String> {
    if plan.preview.mode != "BrowserDirect" {
        return None;
    }
    let url = source
        .parsed
        .url
        .clone()
        .or_else(|| plan.media.source.clone())?;
    let reachable = url.starts_with("http://") || url.starts_with("https://");
    let loopback = url.contains("//127.0.0.1") || url.contains("//localhost");
    (reachable && !loopback).then_some(url)
}

#[component]
fn Head(
    meta: Meta,
    plan: RwSignal<Option<Load<Plan>>>,
    selected: RwSignal<Option<Source>>,
    busy: RwSignal<bool>,
    #[prop(into)] on_play: Callback<()>,
    #[prop(into)] on_preview: Callback<()>,
) -> impl IntoView {
    let art = meta
        .background
        .clone()
        .or_else(|| meta.poster.clone())
        .map(|url| format!("background-image:url('{url}')"))
        .unwrap_or_default();
    let poster = meta
        .poster
        .clone()
        .map(|url| format!("background-image:url('{url}')"))
        .unwrap_or_default();
    let mut facts: Vec<String> = Vec::new();
    if let Some(year) = meta.release_info.clone() {
        facts.push(year);
    }
    if let Some(runtime) = meta.runtime.clone() {
        facts.push(runtime);
    }
    if let Some(rating) = meta.imdb_rating.clone() {
        facts.push(format!("IMDb {rating}"));
    }
    facts.extend(meta.genres.iter().take(3).cloned());
    let director = (!meta.director.is_empty()).then(|| meta.director.join(", "));
    let cast = (!meta.cast.is_empty()).then(|| meta.cast.iter().take(4).cloned().collect::<Vec<_>>().join(", "));

    let can_play = move || selected.get().is_some_and(|source| source.parsed.playable);
    let preview_ready = move || {
        match (plan.get(), selected.get()) {
            (Some(Load::Ready(found)), Some(source)) => {
                previewable_url(&found, &source).is_some()
            }
            _ => false,
        }
    };

    view! {
        <div class="detail-art" style=art></div>
        <div class="detail-grid">
            <div class="detail-poster" style=poster></div>
            <div class="detail-body">
                <h1 class="detail-title">{meta.name.clone()}</h1>
                <div class="chips">
                    {facts
                        .into_iter()
                        .map(|fact| view! { <Chip text=fact /> })
                        .collect_view()}
                </div>
                {meta
                    .description
                    .clone()
                    .map(|text| view! { <p class="detail-desc">{text}</p> })}
                {director
                    .map(|names| {
                        view! { <p class="detail-desc">{format!("Yönetmen: {names}")}</p> }
                    })}
                {cast
                    .map(|names| {
                        view! { <p class="detail-desc">{format!("Oyuncular: {names}")}</p> }
                    })}
                <div class="hero-actions">
                    <Action
                        label=move || if busy.get() { "Gönderiliyor…" } else { "Kodi'de Oynat" }
                            .to_string()
                        variant="primary"
                        autofocus=true
                        disabled=Signal::derive(move || busy.get() || !can_play())
                        on_press=on_play
                    />
                    <Action
                        label="Ön İzle"
                        variant="ghost"
                        disabled=Signal::derive(move || !preview_ready())
                        on_press=on_preview
                    />
                </div>
            </div>
        </div>
    }
}

#[component]
fn SourceList(
    sources: RwSignal<Load<Vec<Source>>>,
    selected: RwSignal<Option<Source>>,
    #[prop(into)] on_pick: Callback<Source>,
) -> impl IntoView {
    view! {
        <div class="sources">
            <h2>"Kaynaklar"</h2>
            {move || match sources.get() {
                Load::Loading => {
                    view! { <div class="state"><strong>"Kaynaklar aranıyor…"</strong></div> }
                        .into_any()
                }
                Load::Failed(detail) => {
                    view! { <Failure title="Kaynaklar alınamadı" detail=detail /> }.into_any()
                }
                Load::Ready(found) if found.is_empty() => {
                    view! {
                        <div class="state">
                            <strong>"Bu başlık için kaynak yok"</strong>
                            <p>"Kurulu eklentilerin hiçbiri bu başlık için akış döndürmedi."</p>
                        </div>
                    }
                        .into_any()
                }
                Load::Ready(found) => {
                    view! {
                        <div class="source-list">
                            {found
                                .into_iter()
                                .map(|source| {
                                    let picked = source.clone();
                                    let identity = source.parsed.identity.clone();
                                    let is_selected = move || {
                                        selected
                                            .get()
                                            .is_some_and(|current| {
                                                current.parsed.identity == identity
                                            })
                                    };
                                    view! {
                                        <button
                                            class="source"
                                            data-focus="1"
                                            tabindex="-1"
                                            aria-selected=move || is_selected().to_string()
                                            on:click=move |_| on_pick.run(picked.clone())
                                        >
                                            <span class="source-name">
                                                {source.parsed.label()}
                                            </span>
                                            <KindChip stream=source.parsed.clone() />
                                            {source
                                                .parsed
                                                .detail()
                                                .map(|text| {
                                                    view! { <span class="source-note">{text}</span> }
                                                })}
                                            {availability_note(source.parsed.clone())}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                    }
                        .into_any()
                }
            }}
        </div>
    }
}

#[component]
fn KindChip(stream: Stream) -> impl IntoView {
    let (text, tone) = match stream.kind.as_str() {
        "http" => ("Doğrudan HTTP", "good"),
        "torrent" => ("Torrent", "warn"),
        "external" => ("Harici servis", "bad"),
        "youtube" => ("YouTube", ""),
        other => (other, ""),
    };
    view! { <Chip text=text.to_string() tone=tone.to_string() /> }
}

/// Say plainly when a listed source is not something this appliance can play.
fn availability_note(stream: Stream) -> Option<impl IntoView> {
    let message = match stream.kind.as_str() {
        "external" => Some(
            "Bu bir abonelik/kiralama bağlantısı. MediaBox bu servisi cihazda oynatamaz."
                .to_string(),
        ),
        "torrent" if !stream.playable => {
            Some("Bu torrent kaynağı çözümlenemedi.".to_string())
        }
        "torrent" => Some(
            "Torrent kaynağı: oynatma, eş bağlantısı kurulabilmesine bağlıdır. \
             Ağ engelliyse veri akmaz."
                .to_string(),
        ),
        _ if !stream.playable => Some("Bu kaynak oynatılabilir değil.".to_string()),
        _ => None,
    }?;
    let tone = if stream.playable { "warn" } else { "bad" };
    Some(view! { <span class=format!("source-note {tone}")>{message}</span> })
}

#[component]
fn Technical(state: Load<Plan>) -> impl IntoView {
    match state {
        Load::Loading => view! {
            <div class="sources">
                <div class="state"><strong>"Kaynak inceleniyor…"</strong>
                    <p>"Dosya başlıkları okunuyor ve bu karta göre karar veriliyor."</p>
                </div>
            </div>
        }
        .into_any(),
        Load::Failed(detail) => view! {
            <div class="sources">
                <div class="state bad">
                    <strong>"Kaynak incelenemedi"</strong>
                    <p>{detail}</p>
                    <p>
                        "Kaynak çözümlendi fakat veri okunamadı. Torrent kaynaklarında bu, \
                         eşlerden hiç parça gelmediği anlamına gelir."
                    </p>
                </div>
            </div>
        }
        .into_any(),
        Load::Ready(plan) => view! { <TechnicalPlan plan=plan /> }.into_any(),
    }
}

#[component]
fn TechnicalPlan(plan: Plan) -> impl IntoView {
    let (verdict, verdict_tone) = plan.verdict();
    let video = plan.video().cloned();
    let audio = plan.audio().cloned();
    let container = plan.media.container.clone();

    let mut chips: Vec<(String, String)> = vec![(verdict.to_string(), verdict_tone.to_string())];
    if let Some(track) = video.as_ref() {
        if let (Some(width), Some(height)) = (track.width, track.height) {
            chips.push((resolution_label(width, height), String::new()));
        }
        if let Some(codec) = track.codec.clone() {
            let label = match track.profile.clone() {
                Some(profile) => format!("{} {profile}", codec.to_uppercase()),
                None => codec.to_uppercase(),
            };
            chips.push((label, String::new()));
        }
        if let Some(depth) = track.bit_depth {
            chips.push((format!("{depth}-bit"), String::new()));
        }
        if let Some(fps) = track.fps {
            chips.push((format!("{fps:.3} fps"), String::new()));
        }
        chips.push(hdr_chip(track));
    }
    if let Some(format) = container.as_ref().and_then(|c| c.format.clone()) {
        chips.push((container_label(&format), String::new()));
    }
    if let Some(track) = audio.as_ref() {
        let codec = track.codec.clone().unwrap_or_else(|| "ses".into()).to_uppercase();
        let layout = track
            .channel_layout
            .clone()
            .or_else(|| track.channels.map(|count| format!("{count} kanal")))
            .unwrap_or_default();
        chips.push((format!("{codec} {layout}").trim().to_string(), String::new()));
    }

    let audio_note = audio_note(&plan);
    let reasons: Vec<Reason> = plan
        .playback
        .reasons
        .iter()
        .chain(plan.playback.video.iter().flat_map(|d| d.reasons.iter()))
        .chain(plan.playback.audio.iter().flat_map(|d| d.reasons.iter()))
        .chain(plan.media.warnings.iter())
        .cloned()
        .collect();
    let preview_reasons = plan.preview.reasons.clone();
    let duration = container.as_ref().and_then(|c| c.duration_seconds);
    let size = container.as_ref().and_then(|c| c.size_bytes);
    let bitrate = container.as_ref().and_then(|c| c.bit_rate);

    view! {
        <div class="sources">
            <h2>"Seçili kaynak"</h2>
            <div class="chips">
                {chips
                    .into_iter()
                    .map(|(text, tone)| view! { <Chip text=text tone=tone /> })
                    .collect_view()}
            </div>
            {audio_note
                .map(|(text, tone)| {
                    view! { <p class=format!("source-note {tone}")>{text}</p> }
                })}
            <div class="rows">
                {duration
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Süre"</span>
                                <span class="row-value">
                                    {seconds_to_clock(value as u64)}
                                </span>
                            </div>
                        }
                    })}
                {size
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Boyut"</span>
                                <span class="row-value">{human_size(value)}</span>
                            </div>
                        }
                    })}
                {bitrate
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Bit hızı"</span>
                                <span class="row-value">{human_bitrate(value)}</span>
                            </div>
                        }
                    })}
                {video
                    .as_ref()
                    .and_then(|track| track.color_transfer.clone())
                    .map(|value| {
                        view! {
                            <div class="row">
                                <span class="row-label">"Transfer / birincil renkler"</span>
                                <span class="row-value">
                                    {format!(
                                        "{value} / {}",
                                        video
                                            .as_ref()
                                            .and_then(|t| t.color_primaries.clone())
                                            .unwrap_or_else(|| "?".into()),
                                    )}
                                </span>
                            </div>
                        }
                    })}
            </div>
            <ul class="reasons">
                {reasons
                    .into_iter()
                    .map(|reason| {
                        let tone = severity_class(&reason.severity);
                        view! { <li class=tone>{reason.message}</li> }
                    })
                    .collect_view()}
                {preview_reasons
                    .into_iter()
                    .map(|reason| {
                        let tone = severity_class(&reason.severity);
                        view! {
                            <li class=tone>{format!("Ön izleme: {}", reason.message)}</li>
                        }
                    })
                    .collect_view()}
            </ul>
        </div>
    }
}

fn severity_class(severity: &str) -> &'static str {
    match severity {
        "blocking" | "error" => "bad",
        "warning" => "warn",
        _ => "",
    }
}

fn container_label(format: &str) -> String {
    match format {
        "mov,mp4,m4a,3gp,3g2,mj2" => "MP4".to_string(),
        "matroska,webm" | "matroska" => "MKV".to_string(),
        "mpegts" | "mpegts,hls" => "MPEG-TS".to_string(),
        other => other.to_uppercase(),
    }
}

/// The HDR badge.
///
/// Dolby Vision is the case that must never be softened. This board has no
/// Dolby Vision pipeline, and a profile 5 stream carries no HDR10 base layer
/// at all — drawing it as "HDR10" would promise a picture the television will
/// never receive, so it is labelled for what it is.
fn hdr_chip(track: &VideoTrack) -> (String, String) {
    if let Some(dv) = track.dolby_vision.as_ref() {
        let profile = dv.profile.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
        let cross_compatible = dv.bl_signal_compatibility_id.is_some_and(|id| id != 0);
        return if cross_compatible {
            (
                format!("Dolby Vision Profil {profile} — DV katmanı yok sayılır"),
                "warn".into(),
            )
        } else {
            (
                format!("Dolby Vision Profil {profile} — desteklenmiyor"),
                "bad".into(),
            )
        };
    }
    match track.hdr.as_deref() {
        Some("HDR10") => ("HDR10".into(), "hot".into()),
        Some("HDR10Plus") | Some("HDR10+") => ("HDR10+".into(), "hot".into()),
        Some("HLG") => ("HLG".into(), "hot".into()),
        Some("SDR") => ("SDR".into(), String::new()),
        Some(other) => (other.to_string(), String::new()),
        None => ("HDR bilgisi yok".into(), String::new()),
    }
}

/// What happens to the audio, in one sentence.
fn audio_note(plan: &Plan) -> Option<(String, &'static str)> {
    let decision = plan.playback.audio.as_ref()?;
    let track = decision.track.as_ref();
    let object = track.and_then(|t| t.object_audio.clone());
    match decision.action.as_str() {
        "Passthrough" => Some(("Ses bit-perfect olarak alıcıya iletilir.".into(), "")),
        "DecodeToPCM" => Some((
            "Ses oynatıcıda PCM'e çözülür; kayıpsız bir aktarım yapılmaz.".into(),
            "",
        )),
        "TranscodeToAC3" => {
            let lost = object
                .map(|format| {
                    format!(
                        "{format} nesne tabanlı ses AC-3 5.1'e indirgenir ve nesne katmanı kaybolur."
                    )
                })
                .unwrap_or_else(|| {
                    "Ses AC-3 5.1'e dönüştürülür; özgün kayıpsız akış korunmaz.".into()
                });
            Some((format!("{lost} Video kopyalanır, yeniden kodlanmaz."), "warn"))
        }
        "Unsupported" => Some(("Bu ses akışı bu cihazda oynatılamaz.".into(), "bad")),
        _ => None,
    }
}

#[component]
fn PreviewSheet(url: String, #[prop(into)] on_close: Callback<()>) -> impl IntoView {
    view! {
        <div class="overlay" data-focus-scope="1">
            <div class="sheet">
                <h2>"Ön İzleme"</h2>
                <p class="panel-note">
                    "Bu, tarayıcıda doğrudan açılan kaynağın kendisidir. Televizyon oynatması \
                     için kapatıp \"Kodi'de Oynat\" seçeneğini kullanın."
                </p>
                <video src=url controls autoplay playsinline></video>
                <div class="actions-row">
                    <Action
                        label="Kapat"
                        variant="primary"
                        autofocus=true
                        on_press=Callback::new(move |()| on_close.run(()))
                    />
                </div>
            </div>
        </div>
    }
}
