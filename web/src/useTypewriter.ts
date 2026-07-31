import { useEffect, useRef, useState } from 'react'

const CHAR_MS = 14
const MAX_CHARS = 200
const CAP_MS = 1500

export const prefersReducedMotion = () =>
  typeof window !== 'undefined' &&
  typeof window.matchMedia === 'function' &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches

const commonPrefixLen = (a: string, b: string) => {
  let i = 0
  const n = Math.min(a.length, b.length)
  while (i < n && a[i] === b[i]) i++
  return i
}

/**
 * Drives per-field typewriter fills for the draft editor. When a remote PATCH
 * changes a text field the operator isn't touching, we type the delta in rather
 * than snapping it — keyed by field name so several fields animate independently.
 * Honors prefers-reduced-motion (sets instantly, no active flag), snaps a pure
 * deletion, and snaps to the newest value if a field is superseded mid-animation.
 * `apply` is the field's state setter; only non-focused fields are ever animated.
 */
export function useTypewriter() {
  const timers = useRef(new Map<string, ReturnType<typeof setInterval>>())
  const [active, setActive] = useState<Set<string>>(new Set())

  const clearActive = (key: string) =>
    setActive((s) => {
      if (!s.has(key)) return s
      const n = new Set(s)
      n.delete(key)
      return n
    })
  const stop = (key: string) => {
    const t = timers.current.get(key)
    if (t !== undefined) {
      clearInterval(t)
      timers.current.delete(key)
    }
  }

  const typewrite = (
    key: string,
    from: string,
    to: string,
    apply: (v: string) => void,
  ) => {
    if (prefersReducedMotion() || timers.current.has(key)) {
      stop(key)
      clearActive(key)
      apply(to)
      return
    }
    const start = commonPrefixLen(from, to)
    if (to.length <= start) {
      apply(to)
      return
    }
    const end = Math.min(to.length, start + MAX_CHARS)
    const perTick = Math.max(1, Math.ceil((end - start) / (CAP_MS / CHAR_MS)))
    let pos = start
    setActive((s) => new Set(s).add(key))
    apply(to.slice(0, pos))
    const id = setInterval(() => {
      pos += perTick
      if (pos >= end) {
        stop(key)
        clearActive(key)
        apply(to)
      } else {
        apply(to.slice(0, pos))
      }
    }, CHAR_MS)
    timers.current.set(key, id)
  }

  useEffect(
    () => () => {
      timers.current.forEach((t) => clearInterval(t))
      timers.current.clear()
    },
    [],
  )

  return { textActive: active, typewrite }
}
