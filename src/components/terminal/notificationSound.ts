let audioCtx: AudioContext | null = null;

function getAudioContext(): AudioContext {
  if (!audioCtx) {
    audioCtx = new AudioContext();
  }
  return audioCtx;
}

/**
 * Play a short, pleasant notification tone using Web Audio API.
 * Two-note chime: ascending minor third, ~200ms total.
 */
export function playNeedsInputChime(): void {
  try {
    const ctx = getAudioContext();
    const now = ctx.currentTime;

    // First note: C5 (523 Hz)
    const osc1 = ctx.createOscillator();
    const gain1 = ctx.createGain();
    osc1.type = "sine";
    osc1.frequency.value = 523;
    gain1.gain.setValueAtTime(0.15, now);
    gain1.gain.exponentialRampToValueAtTime(0.01, now + 0.15);
    osc1.connect(gain1);
    gain1.connect(ctx.destination);
    osc1.start(now);
    osc1.stop(now + 0.15);

    // Second note: E5 (659 Hz), slightly delayed
    const osc2 = ctx.createOscillator();
    const gain2 = ctx.createGain();
    osc2.type = "sine";
    osc2.frequency.value = 659;
    gain2.gain.setValueAtTime(0, now + 0.08);
    gain2.gain.linearRampToValueAtTime(0.12, now + 0.1);
    gain2.gain.exponentialRampToValueAtTime(0.01, now + 0.25);
    osc2.connect(gain2);
    gain2.connect(ctx.destination);
    osc2.start(now + 0.08);
    osc2.stop(now + 0.25);
  } catch {
    // Audio context may not be available (e.g., before user interaction)
  }
}

/**
 * Play a pleasant ascending two-note chime for task completion.
 * C5 → E5, sine wave, ~250ms total with gentle sustain.
 */
export function playCompletionChime(): void {
  try {
    const ctx = getAudioContext();
    const now = ctx.currentTime;

    // First note: C5 (523 Hz)
    const osc1 = ctx.createOscillator();
    const gain1 = ctx.createGain();
    osc1.type = "sine";
    osc1.frequency.value = 523;
    gain1.gain.setValueAtTime(0.15, now);
    gain1.gain.exponentialRampToValueAtTime(0.01, now + 0.12);
    osc1.connect(gain1);
    gain1.connect(ctx.destination);
    osc1.start(now);
    osc1.stop(now + 0.12);

    // Second note: E5 (659 Hz), slightly delayed with longer sustain
    const osc2 = ctx.createOscillator();
    const gain2 = ctx.createGain();
    osc2.type = "sine";
    osc2.frequency.value = 659;
    gain2.gain.setValueAtTime(0, now + 0.1);
    gain2.gain.linearRampToValueAtTime(0.15, now + 0.12);
    gain2.gain.exponentialRampToValueAtTime(0.01, now + 0.35);
    osc2.connect(gain2);
    gain2.connect(ctx.destination);
    osc2.start(now + 0.1);
    osc2.stop(now + 0.35);
  } catch {
    // Audio context may not be available (e.g., before user interaction)
  }
}

/**
 * Play a short descending alert for error transitions.
 * A4 → E4, triangle wave, ~180ms total.
 */
export function playErrorAlert(): void {
  try {
    const ctx = getAudioContext();
    const now = ctx.currentTime;

    // First note: A4 (440 Hz)
    const osc1 = ctx.createOscillator();
    const gain1 = ctx.createGain();
    osc1.type = "triangle";
    osc1.frequency.value = 440;
    gain1.gain.setValueAtTime(0.12, now);
    gain1.gain.exponentialRampToValueAtTime(0.01, now + 0.1);
    osc1.connect(gain1);
    gain1.connect(ctx.destination);
    osc1.start(now);
    osc1.stop(now + 0.1);

    // Second note: E4 (330 Hz), descending for alert feel
    const osc2 = ctx.createOscillator();
    const gain2 = ctx.createGain();
    osc2.type = "triangle";
    osc2.frequency.value = 330;
    gain2.gain.setValueAtTime(0, now + 0.08);
    gain2.gain.linearRampToValueAtTime(0.12, now + 0.1);
    gain2.gain.exponentialRampToValueAtTime(0.01, now + 0.28);
    osc2.connect(gain2);
    gain2.connect(ctx.destination);
    osc2.start(now + 0.08);
    osc2.stop(now + 0.28);
  } catch {
    // Audio context may not be available (e.g., before user interaction)
  }
}
