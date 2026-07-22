import { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react'

/**
 * Bespoke train header (#164): the Chuggernaut locomotive chugging in place
 * while parallax worlds scroll past behind it, cycling through scenes from an
 * asset manifest (`/train/manifest.json`). Scenes are pure content — art +
 * a manifest entry, no code — with built-in code-generated backdrops (space,
 * source dimension, the Matrix) so the header is alive before real art lands.
 *
 * Restraint & a11y: all motion is transform/opacity-only CSS (near-zero CPU);
 * `prefers-reduced-motion`, a hidden tab (visibilitychange), and scrolling the
 * header out of view (IntersectionObserver) all freeze it. Clicking the loco
 * advances to the next scene; the current scene persists across reloads.
 */

type Layer = { src?: string; builtin?: string; speed?: number; y?: number }
type Scene = { name: string; layers?: Layer[]; builtin?: string; tint?: number; duration?: number }
type Manifest = { locomotive?: { src?: string | null }; sceneDurationMs?: number; scenes: Scene[] }

// Ships even if the manifest fetch fails (offline dev, missing file): the same
// three built-in scenes the manifest declares.
const FALLBACK: Manifest = {
  sceneDurationMs: 78000,
  scenes: [
    { name: 'deep space', builtin: 'space', tint: 0.55 },
    { name: 'source dimension', builtin: 'sourcecode', tint: 0.5 },
    { name: 'the matrix', builtin: 'matrix', tint: 0.6 },
  ],
}

const SCENE_KEY = 'chug-train-scene'

function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(
    () => typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches,
  )
  useEffect(() => {
    if (typeof matchMedia !== 'function') return
    const mq = matchMedia('(prefers-reduced-motion: reduce)')
    const on = () => setReduced(mq.matches)
    mq.addEventListener('change', on)
    return () => mq.removeEventListener('change', on)
  }, [])
  return reduced
}

export function TrainHeader() {
  const [manifest, setManifest] = useState<Manifest>(FALLBACK)
  const [idx, setIdx] = useState(() => {
    const n = Number(localStorage.getItem(SCENE_KEY))
    return Number.isFinite(n) && n >= 0 ? n : 0
  })
  // The outgoing scene, kept mounted briefly so the incoming one crossfades over it.
  const [prev, setPrev] = useState<number | null>(null)
  const rootRef = useRef<HTMLDivElement>(null)
  const reduced = usePrefersReducedMotion()
  const [hidden, setHidden] = useState(() => (typeof document !== 'undefined' ? document.hidden : false))
  const [offscreen, setOffscreen] = useState(false)
  const paused = hidden || offscreen || reduced

  useEffect(() => {
    let ok = true
    fetch('/train/manifest.json')
      .then((r) => (r.ok ? r.json() : Promise.reject()))
      .then((m: Manifest) => {
        if (ok && Array.isArray(m?.scenes) && m.scenes.length) setManifest(m)
      })
      .catch(() => {
        /* keep FALLBACK */
      })
    return () => {
      ok = false
    }
  }, [])

  const scenes = manifest.scenes
  const count = scenes.length
  const cur = count ? idx % count : 0

  const advance = useCallback(() => {
    setIdx((i) => {
      const from = count ? i % count : 0
      const next = count ? (from + 1) % count : 0
      setPrev(from === next ? null : from)
      localStorage.setItem(SCENE_KEY, String(next))
      return next
    })
  }, [count])

  // Scene cycling — only while active (never under reduced-motion / hidden / offscreen).
  useEffect(() => {
    if (paused || count < 2) return
    const dwell = scenes[cur]?.duration ?? manifest.sceneDurationMs ?? 78000
    const id = window.setInterval(advance, dwell)
    return () => window.clearInterval(id)
  }, [paused, count, cur, scenes, manifest.sceneDurationMs, advance])

  // Retire the crossfading-out scene once the fade completes.
  useEffect(() => {
    if (prev === null) return
    const id = window.setTimeout(() => setPrev(null), 1500)
    return () => window.clearTimeout(id)
  }, [prev])

  useEffect(() => {
    const onVis = () => setHidden(document.hidden)
    document.addEventListener('visibilitychange', onVis)
    return () => document.removeEventListener('visibilitychange', onVis)
  }, [])

  useEffect(() => {
    const el = rootRef.current
    if (!el || typeof IntersectionObserver !== 'function') return
    const io = new IntersectionObserver(([e]) => setOffscreen(!e.isIntersecting), { threshold: 0 })
    io.observe(el)
    return () => io.disconnect()
  }, [])

  const scene = scenes[cur]
  const tint = scene?.tint ?? 0.5

  return (
    <div
      ref={rootRef}
      className={`train${paused ? ' train-paused' : ''}`}
      data-scene={scene?.builtin ?? 'art'}
      style={{ ['--scrim']: String(tint) } as CSSProperties}
      aria-hidden="true"
    >
      <div className="train-worlds">
        {prev !== null && scenes[prev] && <SceneView key={`p-${prev}`} scene={scenes[prev]} fading />}
        {scene && <SceneView key={`c-${cur}`} scene={scene} />}
      </div>
      <Locomotive onClick={advance} />
      <div className="train-scrim" />
      <span className="train-scene-name">{scene?.name}</span>
    </div>
  )
}

function SceneView({ scene, fading }: { scene: Scene; fading?: boolean }) {
  const layers: Layer[] = scene.layers?.length
    ? scene.layers
    : builtinLayers(scene.builtin ?? 'space')
  const builtin = !scene.layers?.length ? scene.builtin ?? 'space' : undefined
  return (
    <div
      className={`train-scene${builtin ? ` sc-${builtin}` : ' sc-art'}${
        fading ? ' train-scene-out' : ' train-scene-in'
      }`}
    >
      {layers.map((l, i) => (
        <ParallaxLayer key={i} layer={l} depth={i} builtin={builtin} />
      ))}
    </div>
  )
}

// Built-in code-generated layer sets (far -> near). Real art overrides these by
// supplying `layers` in the manifest, so the loop stays content-agnostic.
function builtinLayers(kind: string): Layer[] {
  if (kind === 'matrix') return [{ builtin: 'matrix', speed: 0.15 }]
  if (kind === 'sourcecode') return [{ speed: 0.1 }, { speed: 0.3 }, { speed: 0.6 }]
  return [{ speed: 0.08 }, { speed: 0.22 }, { speed: 0.5 }] // space
}

function ParallaxLayer({ layer, depth, builtin }: { layer: Layer; depth: number; builtin?: string }) {
  // Slower layer -> longer scroll period (far things drift; near things rush).
  const speed = layer.speed ?? 0.3
  const dur = Math.max(8, 120 * (1 - speed))
  const style = { ['--dur']: `${dur}s`, top: layer.y ?? 0 } as CSSProperties
  if (layer.src) {
    return (
      <div className="train-layer train-scroll" style={style}>
        <div className="train-strip" style={{ backgroundImage: `url(${layer.src})` }} />
      </div>
    )
  }
  if (builtin === 'matrix' || layer.builtin === 'matrix') return <MatrixRain />
  return (
    <div className={`train-layer train-scroll train-${builtin ?? 'space'}-${depth}`} style={style}>
      <div className="train-strip" />
    </div>
  )
}

// The Matrix: THE green digital-glyph rain (distinct from the abstract source
// dimension). Columns fall on staggered loops; transform/opacity-only.
const GLYPHS = 'ｦｱｳｴｵｶｷｸｹｺｻｼｽｾﾀﾁﾂﾃ01ﾊﾋﾌﾍﾎ<>{}=;/'
function MatrixRain() {
  const [cols] = useState(() => {
    const N = 16
    return Array.from({ length: N }, (_, i) => {
      let text = ''
      for (let k = 0; k < 18; k++) text += GLYPHS[(i * 7 + k * 13) % GLYPHS.length]
      return { text, dur: 5 + ((i * 37) % 9), delay: -((i * 53) % 11), left: (i / N) * 100 }
    })
  })
  return (
    <div className="train-layer mx-rain">
      {cols.map((c, i) => (
        <span
          key={i}
          className="mx-col"
          style={{ left: `${c.left}%`, animationDuration: `${c.dur}s`, animationDelay: `${c.delay}s` }}
        >
          {c.text.split('').map((ch, j) => (
            <span key={j}>{ch}</span>
          ))}
        </span>
      ))}
    </div>
  )
}

// The locomotive: sits left-of-centre, wheels turning, an occasional smoke puff.
// A hidden delight — click it to jump to the next world.
function Locomotive({ onClick }: { onClick: () => void }) {
  return (
    <button type="button" className="loco" onClick={onClick} title="chug on to the next world" aria-label="Next scene">
      <span className="loco-smoke loco-smoke-1" />
      <span className="loco-smoke loco-smoke-2" />
      <span className="loco-smoke loco-smoke-3" />
      <svg className="loco-svg" viewBox="0 0 150 90" width="150" height="90" fill="none" aria-hidden="true">
        {/* body */}
        <path d="M12 62V34a4 4 0 0 1 4-4h58l14 18h20a6 6 0 0 1 6 6v8H12Z" className="loco-body" />
        <rect x="18" y="20" width="30" height="16" rx="3" className="loco-cab" />
        <rect x="94" y="30" width="12" height="18" rx="2" className="loco-cab" />
        {/* chimney */}
        <rect x="30" y="10" width="12" height="12" rx="2" className="loco-chimney" />
        <ellipse cx="36" cy="10" rx="9" ry="4" className="loco-chimney" />
        {/* headlamp */}
        <circle cx="120" cy="52" r="4" className="loco-lamp" />
        {/* wheels */}
        <g className="loco-wheel">
          <circle cx="34" cy="68" r="13" className="loco-wheel-rim" />
          <line x1="34" y1="57" x2="34" y2="79" className="loco-spoke" />
          <line x1="23" y1="68" x2="45" y2="68" className="loco-spoke" />
          <line x1="26" y1="60" x2="42" y2="76" className="loco-spoke" />
          <line x1="26" y1="76" x2="42" y2="60" className="loco-spoke" />
        </g>
        <g className="loco-wheel">
          <circle cx="92" cy="68" r="13" className="loco-wheel-rim" />
          <line x1="92" y1="57" x2="92" y2="79" className="loco-spoke" />
          <line x1="81" y1="68" x2="103" y2="68" className="loco-spoke" />
          <line x1="84" y1="60" x2="100" y2="76" className="loco-spoke" />
          <line x1="84" y1="76" x2="100" y2="60" className="loco-spoke" />
        </g>
        {/* connecting rod */}
        <line x1="34" y1="68" x2="92" y2="68" className="loco-rod" />
      </svg>
    </button>
  )
}
