import { StrictMode, Suspense, lazy } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter, Routes, Route, Navigate, useMatch } from 'react-router-dom'
import { AppShell } from './components/AppShell'
import { ProjectHeader } from './components/ProjectHeader'
import { Skeleton, SkeletonLines } from './components/Skeleton'
import { ThemePicker, applySavedTheme } from './theme'
import './styles.css'

// Every page is code-split. Imported eagerly they were one 479 KB chunk on every
// load, and the markdown renderer — the biggest slice of it — is only needed by
// the three pages that render agent prose. Now the entry chunk is the shell,
// router and theme; a route's code (and whatever only it depends on) arrives
// when you navigate to it. Pages export named components, so each import maps
// its export onto `default` for lazy().
const Login = lazy(() => import('./pages/Login').then((m) => ({ default: m.Login })))
const Home = lazy(() => import('./pages/Home').then((m) => ({ default: m.Home })))
const SharePage = lazy(() => import('./pages/Share').then((m) => ({ default: m.SharePage })))
const PlatformSettingsPage = lazy(() =>
  import('./pages/PlatformSettings').then((m) => ({ default: m.PlatformSettingsPage })),
)
const ClusterPage = lazy(() => import('./pages/Cluster').then((m) => ({ default: m.ClusterPage })))
const ProjectPage = lazy(() => import('./pages/Project').then((m) => ({ default: m.ProjectPage })))
const DeploysPage = lazy(() => import('./pages/Deploys').then((m) => ({ default: m.DeploysPage })))
const LibraryPage = lazy(() => import('./pages/Library').then((m) => ({ default: m.LibraryPage })))
const JobTypePage = lazy(() => import('./pages/Library').then((m) => ({ default: m.JobTypePage })))
const NewJobPage = lazy(() => import('./pages/NewJob').then((m) => ({ default: m.NewJobPage })))
const StatsPage = lazy(() => import('./pages/Stats').then((m) => ({ default: m.StatsPage })))
const PromptsPage = lazy(() => import('./pages/Prompts').then((m) => ({ default: m.PromptsPage })))
const TagsPage = lazy(() => import('./pages/Tags').then((m) => ({ default: m.TagsPage })))
const DesignsPage = lazy(() => import('./pages/Designs').then((m) => ({ default: m.DesignsPage })))
const DesignPage = lazy(() => import('./pages/Designs').then((m) => ({ default: m.DesignPage })))
const FileViewPage = lazy(() => import('./pages/FileView').then((m) => ({ default: m.FileViewPage })))
const SettingsPage = lazy(() => import('./pages/Settings').then((m) => ({ default: m.SettingsPage })))
const JobDetail = lazy(() => import('./pages/JobDetail').then((m) => ({ default: m.JobDetail })))

applySavedTheme()

// Register the service worker so the UI is installable as a PWA (Android/Chrome
// require a registered SW) and gets a basic offline app-shell. Dev runs skip it
// to avoid the SW caching HMR assets.
if ('serviceWorker' in navigator && import.meta.env.PROD) {
  window.addEventListener('load', () => {
    navigator.serviceWorker.register('/sw.js').catch(() => {})
  })
}

// Shown while a route's chunk is in flight. It has to render the *same chrome*
// the arriving page renders, not just a card: a project page opens with
// ProjectHeader (masthead + tabs), so a headerless fallback would float its
// first card at the top of the viewport and then drop it a header's height when
// the chunk lands — a layout shift on exactly the cold, slow load that
// code-splitting introduced, and worst on a phone. So the fallback carries
// the header whenever the URL is project-scoped, and the card skeleton below it
// is the page → card → heading + lines shape every page already skeletons with.
// Nothing here has an intrinsic width, so it can't scroll a narrow viewport
// sideways. ProjectHeader lands in the entry chunk by being imported here; it is
// small and every project route needs it anyway.
function RouteFallback() {
  const inProject = useMatch('/p/:owner/:project/*')
  const { owner = '', project = '' } = inProject?.params ?? {}
  return (
    <div className="page">
      {inProject && <ProjectHeader owner={owner} project={project} />}
      <section className="card">
        <Skeleton width="12rem" height="1.3em" />
        <SkeletonLines n={4} />
      </section>
    </div>
  )
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemePicker />
    <BrowserRouter>
      <AppShell>
      <Suspense fallback={<RouteFallback />}>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route path="/" element={<Home />} />
        {/* PWA share-target landing: the OS share sheet drops a screenshot here. */}
        <Route path="/share" element={<SharePage />} />
        <Route path="/settings" element={<PlatformSettingsPage />} />
        <Route path="/cluster" element={<ClusterPage />} />
        {/* Deploys are per-project now; the old platform drift/freshness view
            folded into Cluster. Redirect stale bookmarks there. */}
        <Route path="/deploys" element={<Navigate to="/cluster" replace />} />
        <Route path="/p/:owner/:project" element={<ProjectPage />} />
        <Route path="/p/:owner/:project/deploys" element={<DeploysPage />} />
        <Route path="/p/:owner/:project/job-types" element={<LibraryPage />} />
        <Route path="/p/:owner/:project/job-types/:name" element={<JobTypePage />} />
        {/* legacy alias */}
        <Route path="/p/:owner/:project/library" element={<LibraryPage />} />
        <Route path="/p/:owner/:project/library/:name" element={<JobTypePage />} />
        <Route path="/p/:owner/:project/jobs/new" element={<NewJobPage />} />
        <Route path="/p/:owner/:project/stats" element={<StatsPage />} />
        <Route path="/p/:owner/:project/prompts" element={<PromptsPage />} />
        <Route path="/p/:owner/:project/tags" element={<TagsPage />} />
        <Route path="/p/:owner/:project/designs" element={<DesignsPage />} />
        <Route path="/p/:owner/:project/designs/:slug" element={<DesignPage />} />
        <Route path="/p/:owner/:project/files" element={<FileViewPage />} />
        <Route path="/p/:owner/:project/settings" element={<SettingsPage />} />
        <Route path="/p/:owner/:project/jobs/:seq" element={<JobDetail />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
      </Suspense>
      </AppShell>
    </BrowserRouter>
  </StrictMode>,
)
