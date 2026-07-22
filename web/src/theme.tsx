import { useState } from 'react'

// Names must match the [data-theme='...'] blocks in styles.css. 'cosmos' is the
// snazzy-redesign dark-first identity (#161); 'aurora' its light sibling.
export const THEMES = ['cosmos', 'aurora', 'tokyo', 'midnight', 'gruvbox', 'nord'] as const
export type Theme = (typeof THEMES)[number]

const STORAGE_KEY = 'chug-theme'

function currentTheme(): Theme {
  const saved = localStorage.getItem(STORAGE_KEY)
  return THEMES.includes(saved as Theme) ? (saved as Theme) : 'cosmos'
}

export function applySavedTheme() {
  document.documentElement.dataset.theme = currentTheme()
}

export function ThemePicker() {
  const [theme, setTheme] = useState<Theme>(currentTheme)
  const pick = (t: Theme) => {
    setTheme(t)
    localStorage.setItem(STORAGE_KEY, t)
    document.documentElement.dataset.theme = t
  }
  return (
    <div className="theme-picker">
      <select
        aria-label="Theme"
        value={theme}
        onChange={(e) => pick(e.target.value as Theme)}
      >
        {THEMES.map((t) => (
          <option key={t} value={t}>
            {t}
          </option>
        ))}
      </select>
    </div>
  )
}
