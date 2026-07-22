// Hand-rolled inline SVG icons for the snazzy redesign (#161). Stroke-based,
// currentColor, 20px grid — no icon library (web/CLAUDE.md: no component libs).
// Each takes an optional size; colour comes from the parent's `color`.

type P = { size?: number }
const svg = (size: number) => ({
  width: size,
  height: size,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 1.8,
  strokeLinecap: 'round' as const,
  strokeLinejoin: 'round' as const,
})

export const IconCode = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <polyline points="16 18 22 12 16 6" />
    <polyline points="8 6 2 12 8 18" />
  </svg>
)
export const IconGrid = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <rect x="3" y="3" width="7" height="7" rx="1.5" />
    <rect x="14" y="3" width="7" height="7" rx="1.5" />
    <rect x="3" y="14" width="7" height="7" rx="1.5" />
    <rect x="14" y="14" width="7" height="7" rx="1.5" />
  </svg>
)
export const IconChat = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <path d="M21 11.5a8.4 8.4 0 0 1-8.5 8.5 8.9 8.9 0 0 1-4-.9L3 20l1.4-4.5A8.4 8.4 0 0 1 3.5 11 8.4 8.4 0 0 1 12 2.5a8.4 8.4 0 0 1 9 9Z" />
  </svg>
)
export const IconTag = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <path d="M20.6 13.4 13.4 20.6a1.9 1.9 0 0 1-2.7 0L3 12.9V3h9.9l7.7 7.7a1.9 1.9 0 0 1 0 2.7Z" />
    <circle cx="7.5" cy="7.5" r="1.4" />
  </svg>
)
export const IconFile = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8Z" />
    <polyline points="14 3 14 8 19 8" />
  </svg>
)
export const IconGear = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-2.9 1.2V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 6 19.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0-1.2-2.9H2a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 3.3 6l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3H8a1.7 1.7 0 0 0 1-1.6V2a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 2.9 1.2l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9V8a1.7 1.7 0 0 0 1.6 1H22a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
  </svg>
)
export const IconHome = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <path d="M3 10.5 12 3l9 7.5" />
    <path d="M5 9.5V20a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V9.5" />
  </svg>
)
export const IconServer = ({ size = 20 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <rect x="3" y="4" width="18" height="7" rx="1.5" />
    <rect x="3" y="13" width="18" height="7" rx="1.5" />
    <line x1="7" y1="7.5" x2="7" y2="7.5" />
    <line x1="7" y1="16.5" x2="7" y2="16.5" />
  </svg>
)
export const IconSearch = ({ size = 16 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <circle cx="11" cy="11" r="7" />
    <line x1="21" y1="21" x2="16.65" y2="16.65" />
  </svg>
)
export const IconFilter = ({ size = 16 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <polygon points="22 3 2 3 10 12.5 10 19 14 21 14 12.5 22 3" />
  </svg>
)
export const IconLock = ({ size = 13 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <rect x="4" y="10" width="16" height="10" rx="2" />
    <path d="M8 10V7a4 4 0 0 1 8 0v3" />
  </svg>
)
export const IconGlobe = ({ size = 13 }: P) => (
  <svg {...svg(size)} aria-hidden="true">
    <circle cx="12" cy="12" r="9" />
    <path d="M3 12h18M12 3a14 14 0 0 1 0 18 14 14 0 0 1 0-18Z" />
  </svg>
)
