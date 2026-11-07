import { createEffect, createSignal, For, on, onCleanup, onMount, Show, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";
import { Columns2, Command, Eye, FileArchive, Inbox, Layers, Network, Palette, Search, Server, Sparkles, Terminal } from "../lib/icons";
import { INTRO_STEPS, type IntroDemo, type IntroPlan, type IntroStep } from "../lib/intro";
import { motionOn } from "../lib/motion";
import { applyAppearance, closeIntro, persistSettings, setSettings, setUi, settings, ui } from "../lib/state";
import { updater } from "../lib/updater";
import { DEMOS } from "./IntroDemos";
import { AccentSwatches, Field, Seg, Toggle, useDefaultManager } from "./SettingsDialog";
import "../intro.css";

const ICONS: Record<IntroDemo, Component<{ size?: number }>> = {
  search: Search, preview: Eye, zip: FileArchive, tabs: Columns2, jobs: Layers, dropzone: Inbox, wsl: Terminal, network: Network, servers: Server, commands: Command, setup: Palette,
};
const iconOf = (s: IntroStep) => (s.demo ? ICONS[s.demo] : Sparkles);

/** `Backticks` in a step's text: something you'd type. */
const withCode = (text: string) =>
  text.split(/(`[^`]+`)/).filter(Boolean).map((part) => (part.startsWith("`") ? <code>{part.slice(1, -1)}</code> : part));

/** What Ferret can do: lib/intro.ts has the steps and when they show. */
export default function Intro() {
  return <Show when={ui.intro} keyed>{(plan) => <Tour plan={plan} />}</Show>;
}

function Tour(props: { plan: IntroPlan }) {
  const steps = props.plan.steps.map((id) => INTRO_STEPS.find((s) => s.id === id)).filter((s): s is IntroStep => !!s);
  const first = props.plan.welcome ? -1 : 0;
  const last = steps.length - 1;
  /** Which step; -1 is the welcome screen. */
  const [at, setAt] = createSignal(first);
  const [forward, setForward] = createSignal(true);
  const step = () => steps[at()];
  let box!: HTMLDivElement;

  /** A button that goes away with the screen it was on leaves the keyboard on the page behind. */
  const holdFocus = () => queueMicrotask(() => { if (!box.contains(document.activeElement)) box.focus(); });
  const go = (i: number) => {
    const to = Math.max(first, Math.min(last, i));
    if (to === at()) return;
    setForward(to > at());
    setAt(to);
    holdFocus();
  };
  const next = () => (at() >= last ? closeIntro(true) : go(at() + 1));
  const leave = () => closeIntro(at() >= last);

  const onKey = (e: KeyboardEvent) => {
    const target = e.target as HTMLElement;
    const inField = !!target.closest("input, select, textarea");
    const ctrl = e.ctrlKey || e.metaKey;
    const k = e.key.toLowerCase();
    if (e.key === "Escape") leave();
    else if (ctrl && k === "k") { e.preventDefault(); setUi("palette", "files"); }
    else if (ctrl && e.shiftKey && k === "p") { e.preventDefault(); setUi("palette", "commands"); }
    else if (inField) { /* its own keys */ }
    else if (e.key === "ArrowRight" || e.key === "ArrowDown" || e.key === "PageDown") { e.preventDefault(); go(at() + 1); }
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp" || e.key === "PageUp") { e.preventDefault(); go(at() - 1); }
    else if (e.key === "Home") { e.preventDefault(); go(first); }
    else if (e.key === "End") { e.preventDefault(); go(last); }
    // On a button, Enter presses that button.
    else if (e.key === "Enter" && target === box) { e.preventDefault(); next(); }
    e.stopPropagation();
  };

  // Something opened over the tour (Try it, or a command run from it) has closed: back to the tour.
  createEffect(on(() => !!(ui.palette || ui.settingsOpen || ui.dialog || ui.quickLook || ui.networkForm || ui.networkLogin), (over, was) => {
    if (was && !over) requestAnimationFrame(() => box.focus());
  }, { defer: true }));

  return (
    // The window can still be moved by the dimmed part around the tour, which never closes it:
    // a stray click shouldn't count as having seen everything.
    <div class="modal-backdrop" data-tauri-drag-region>
      <div class="modal intro" classList={{ one: steps.length === 1 && at() >= 0 }} role="dialog" aria-modal="true"
        aria-label={props.plan.welcome ? "Welcome to Ferret" : "What Ferret can do"} tabIndex={-1} onKeyDown={onKey}
        ref={(el) => { box = el; queueMicrotask(() => el.focus()); }}>
        <Show when={at() >= 0} fallback={<Welcome steps={steps} onStart={() => go(0)} onJump={go} onSkip={leave} />}>
          <Show when={steps.length > 1}>
            <nav class="ix-rail" aria-label="Steps">
              <div class="ix-rail-title">{props.plan.welcome || props.plan.replay ? "What Ferret can do" : "New in Ferret"}</div>
              <For each={steps}>
                {(s, i) => (
                  <button class="side-item" classList={{ active: i() === at() }} aria-current={i() === at() ? "step" : undefined} onClick={() => go(i())}>
                    <Dynamic component={iconOf(s)} size={15} />
                    <span class="side-label">{s.title}</span>
                  </button>
                )}
              </For>
              <div class="ix-rail-foot">
                <img src="/ferret-256.png" alt="" width={20} height={20} />
                <span>Ferret {updater.current}</span>
              </div>
            </nav>
          </Show>
          <div class="ix-main">
            <Show when={step()} keyed>
              {(s) => (
                <div class="ix-step" classList={{ back: !forward() }}>
                  <div class="ix-stage" aria-hidden="true">
                    <Show when={s.demo} fallback={<KeysStage step={s} />}>
                      <Dynamic component={DEMOS[s.demo!]} />
                    </Show>
                  </div>
                  <div class="ix-copy" aria-live="polite">
                    <h2>{s.title}</h2>
                    <p>{withCode(s.text)}</p>
                    <Show when={s.demo === "setup"}><SetupFields /></Show>
                    <Show when={s.keys?.length || s.tryIt}>
                      <div class="ix-keys">
                        <For each={s.keys}>
                          {(k) => <span class="ix-key"><kbd>{k.key}</kbd>{k.does}</span>}
                        </For>
                        <Show when={s.tryIt}>
                          <Show when={s.tryIt === "network"} fallback={
                            <button class="btn ix-try" onClick={() => setUi("palette", s.tryIt as "files" | "commands")}>Try it</button>
                          }>
                            <button class="btn ix-try" onClick={() => setUi("networkForm", true)}>Add one</button>
                          </Show>
                        </Show>
                      </div>
                    </Show>
                  </div>
                </div>
              )}
            </Show>
            <footer class="ix-foot">
              <Show when={at() < last}>
                <button class="btn ix-skip" onClick={leave}>{props.plan.replay ? "Close" : "Skip"}</button>
              </Show>
              <Show when={steps.length === 1}><span class="ix-note">New in Ferret {updater.current}</span></Show>
              <Show when={steps.length > 1}>
                <span class="ix-count">{at() + 1} of {steps.length}</span>
                <Show when={at() > first}><button class="btn" onClick={() => go(at() - 1)}>Back</button></Show>
              </Show>
              <button class="btn accent" onClick={next}>{at() < last ? "Next" : steps.length === 1 ? "Got it" : "Done"}</button>
            </footer>
          </div>
        </Show>
      </div>
    </div>
  );
}

/** First launch: the splash, with everything the tour covers in view, so even a Skip learns something. */
function Welcome(props: { steps: IntroStep[]; onStart: () => void; onJump: (i: number) => void; onSkip: () => void }) {
  // A light coming on, once the window is up: at the very start the window may still be appearing.
  const [lit, setLit] = createSignal(!motionOn());
  onMount(() => {
    const t = setTimeout(() => setLit(true), 180);
    onCleanup(() => clearTimeout(t));
  });
  return (
    <div class="ix-welcome" classList={{ lit: lit() }}>
      <img class="ix-logo" src="/ferret-256.png" alt="" width={104} height={104} />
      <h2>Welcome to Ferret</h2>
      <p>A fast, keyboard-first file manager for Windows. Here's what it can do, in about two minutes.</p>
      <div class="ix-index" style={{ "grid-template-rows": `repeat(${Math.ceil(props.steps.length / 2)}, auto)` }}>
        <For each={props.steps}>
          {(s, i) => (
            <button class="side-item" onClick={() => props.onJump(i())}>
              <Dynamic component={iconOf(s)} size={15} />
              <span class="side-label">{s.title}</span>
            </button>
          )}
        </For>
      </div>
      <div class="ix-welcome-actions">
        <button class="btn accent" onClick={props.onStart}>Show me around</button>
        <button class="btn ix-skip" onClick={props.onSkip}>Skip</button>
      </div>
      <p class="ix-later">You can see this again from Settings.</p>
    </div>
  );
}

/** A step without a demo of its own: its keys, big. */
function KeysStage(props: { step: IntroStep }) {
  return (
    <div class="ix-keys-stage">
      <Show when={props.step.keys?.length} fallback={<img src="/ferret-256.png" alt="" width={88} height={88} />}>
        <For each={props.step.keys}>{(k) => <kbd>{k.key}</kbd>}</For>
      </Show>
    </div>
  );
}

/** The real settings, changing the real window behind the tour. */
function SetupFields() {
  const manager = useDefaultManager(() => true);
  return (
    <div class="ix-fields">
      <Field label="Accent color"><AccentSwatches /></Field>
      <Field label="Theme">
        <Seg value={settings.theme} onChange={(v) => { setSettings("theme", v); persistSettings(); applyAppearance(); }}
          options={[{ value: "dark", label: "Dark" }, { value: "light", label: "Light" }, { value: "auto", label: "System" }]} />
      </Field>
      <Field label="Use Ferret instead of File Explorer" hint="Win+E and folders opened from the desktop and other apps open in Ferret. Turning it off puts Explorer back exactly as it was.">
        <Toggle checked={!!manager.status()?.enabled} onChange={manager.change} />
      </Field>
      <Show when={manager.status()?.devBuild && !manager.status()?.enabled}>
        <p class="card-note">This copy is a development or test build. Turn this on from the installed Ferret.</p>
      </Show>
    </div>
  );
}
