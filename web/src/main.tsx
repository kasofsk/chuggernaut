import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import { Login } from './pages/Login'
import { Home } from './pages/Home'
import { ProjectPage } from './pages/Project'
import { LibraryPage, JobTypePage } from './pages/Library'
import { NewJobPage } from './pages/NewJob'
import { FileViewPage } from './pages/FileView'
import { TagsPage } from './pages/Tags'
import { PromptsPage } from './pages/Prompts'
import { SettingsPage } from './pages/Settings'
import { PlatformSettingsPage } from './pages/PlatformSettings'
import { JobDetail } from './pages/JobDetail'
import { ClusterPage } from './pages/Cluster'
import { AppShell } from './components/AppShell'
import { ThemePicker, applySavedTheme } from './theme'
import './styles.css'

applySavedTheme()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemePicker />
    <BrowserRouter>
      <AppShell>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route path="/" element={<Home />} />
        <Route path="/settings" element={<PlatformSettingsPage />} />
        <Route path="/cluster" element={<ClusterPage />} />
        <Route path="/p/:owner/:project" element={<ProjectPage />} />
        <Route path="/p/:owner/:project/job-types" element={<LibraryPage />} />
        <Route path="/p/:owner/:project/job-types/:name" element={<JobTypePage />} />
        {/* legacy alias */}
        <Route path="/p/:owner/:project/library" element={<LibraryPage />} />
        <Route path="/p/:owner/:project/library/:name" element={<JobTypePage />} />
        <Route path="/p/:owner/:project/jobs/new" element={<NewJobPage />} />
        <Route path="/p/:owner/:project/prompts" element={<PromptsPage />} />
        <Route path="/p/:owner/:project/tags" element={<TagsPage />} />
        <Route path="/p/:owner/:project/files" element={<FileViewPage />} />
        <Route path="/p/:owner/:project/settings" element={<SettingsPage />} />
        <Route path="/p/:owner/:project/jobs/:seq" element={<JobDetail />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
      </AppShell>
    </BrowserRouter>
  </StrictMode>,
)
