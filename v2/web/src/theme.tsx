import { useState } from 'react'

// Names must match the [data-theme='...'] blocks in styles.css.
export const THEMES = ['tokyo', 'midnight', 'gruvbox', 'nord'] as const
export type Theme = (typeof THEMES)[number]

const STORAGE_KEY = 'chug-theme'

function currentTheme(): Theme {
  const saved = localStorage.getItem(STORAGE_KEY)
  return THEMES.includes(saved as Theme) ? (saved as Theme) : 'tokyo'
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
