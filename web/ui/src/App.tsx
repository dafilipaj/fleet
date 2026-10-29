import { Redirect, Route, Router, Switch } from 'wouter'
import { useHashLocation } from 'wouter/use-hash-location'

import { MemoRecorderHost } from '@/components/MemoRecorder'
import { Scratchpad } from '@/components/Scratchpad'
import { StackUi } from '@/components/stack/StackUi'
import { Toaster } from '@/components/ui/sonner'
import { TooltipProvider } from '@/components/ui/tooltip'
import { DormantProvider } from '@/providers/DormantProvider'
import { FleetProvider } from '@/providers/FleetProvider'
import { PrefsProvider } from '@/providers/PrefsProvider'
import { PrsProvider } from '@/providers/PrsProvider'
import { SettingsProvider } from '@/providers/SettingsProvider'
import { SpeechProvider } from '@/providers/SpeechProvider'
import { TasksProvider } from '@/providers/TasksProvider'
import { useAppErrors } from '@/hooks/useAppErrors'
import { usePreventFileNavigation } from '@/hooks/useAttach'
import { useIsDesktop } from '@/hooks/useMediaQuery'
import { ThemeProvider } from '@/providers/ThemeProvider'
import { readEmbed } from '@/lib/embed'
import { DesktopShell } from '@/screens/DesktopShell'
import { EmbedScreen } from '@/screens/EmbedScreen'
import { InboxScreen } from '@/screens/InboxScreen'
import { JobsScreen } from '@/screens/JobsScreen'
import { MemosScreen } from '@/screens/MemosScreen'
import { ListScreen } from '@/screens/ListScreen'
import { NotesScreen } from '@/screens/NotesScreen'
import { OperatorScreen } from '@/screens/OperatorScreen'
import { PrsScreen } from '@/screens/PrsScreen'
import { ReviewScreen } from '@/screens/ReviewScreen'
import { SessionScreen } from '@/screens/SessionScreen'
import { SettingsScreen } from '@/screens/SettingsScreen'
import { TasksScreen } from '@/screens/TasksScreen'
import { UsageScreen } from '@/screens/UsageScreen'

/**
 * Hash routing: `#/` (list), `#/s/<host>/<session_id>` (detail),
 * plus `#/embed?host=…&dir=…` (one directory's sessions, for another app to frame: its own tree, below), `#/settings` (this viewer's preferences), `#/usage` (the subscription limits), `#/inbox` (everything waiting on you), `#/bots` (agent sessions from a recipe; `#/jobs` is its old name), `#/operator` (talk to the fleet by voice), `#/prs` (the GitHub PRs that need you), `#/tasks` (your open Productive tasks), `#/memos[/<id>]` (voice memos) and `#/notes[/<host>[/<path>]]` (the notes explorer) and `#/review/<host>/<session_id>` (the diff reviewer).
 * No server-side SPA fallback needed; the PWA start_url is `/#/`. Desktop and mobile use the
 * same routes, so links work across both.
 */
// Read once: inside the frame the hash moves on to `#/s/…`, the embed tree stays.
const embed = readEmbed()

export default function App() {
  return embed ? <EmbedApp /> : <FleetApp />
}

function FleetApp() {
  // ≥ lg: master–detail shell; below it the mobile screens, untouched. Crossing the
  // breakpoint swaps trees (the hash route is shared, so the open session stays open).
  const desktop = useIsDesktop()
  // A file dropped outside a drop zone must not navigate the app away.
  usePreventFileNavigation()
  // Fleet.app's native actions (the iTerm buttons) report failures as toasts.
  useAppErrors()
  return (
    <ThemeProvider>
      <PrefsProvider>
      <SettingsProvider>
      <SpeechProvider>
        <FleetProvider>
        <DormantProvider>
        <PrsProvider>
        <TasksProvider>
          <TooltipProvider>
            <Router hook={useHashLocation}>
              {desktop ? (
                <DesktopShell />
              ) : (
                <Switch>
                  <Route path="/">
                    <ListScreen />
                  </Route>
                  <Route path="/settings">
                    <SettingsScreen />
                  </Route>
                  <Route path="/usage">
                    <UsageScreen />
                  </Route>
                  <Route path="/inbox">
                    <InboxScreen />
                  </Route>
                  <Route path={/^\/(?:bots|jobs)$/}>
                    <JobsScreen />
                  </Route>
                  <Route path="/operator">
                    <OperatorScreen />
                  </Route>
                  <Route path={/^\/memos(?:\/.*)?$/}>
                    <MemosScreen />
                  </Route>
                  <Route path={/^\/prs(?:\/.*)?$/}>
                    <PrsScreen />
                  </Route>
                  <Route path={/^\/tasks(?:\/.*)?$/}>
                    <TasksScreen />
                  </Route>
                  <Route path={/^\/notes(?:\/.*)?$/}>
                    <NotesScreen />
                  </Route>
                  <Route path="/review/:host/:id">
                    <ReviewScreen />
                  </Route>
                  <Route path="/s/:host/:id">
                    {(p) => <SessionScreen key={`${p.host}/${p.id}`} host={p.host} id={p.id} />}
                  </Route>
                  <Route>
                    <Redirect to="/" replace />
                  </Route>
                </Switch>
              )}
              {/* Session stacks: the Stack sheet + Spawn sibling form, opened from rows, columns, the session screen. */}
              <StackUi />
              {/* ⌘⇧S / header buttons: the floating (desktop) or drawer (mobile) scratchpad. */}
              <Scratchpad />
              {/* Voice memos: recover unsaved recordings, announce saved ones. */}
              <MemoRecorderHost />
            </Router>
            <Toaster position={desktop ? 'bottom-right' : 'top-center'} offset={desktop ? { bottom: 112, right: 16 } : undefined} />
          </TooltipProvider>
        </TasksProvider>
        </PrsProvider>
        </DormantProvider>
        </FleetProvider>
      </SpeechProvider>
      </SettingsProvider>
      </PrefsProvider>
    </ThemeProvider>
  )
}

/** The embed view: the providers a session needs, none of Fleet's navigation or global panels. */
function EmbedApp() {
  usePreventFileNavigation()
  return (
    <ThemeProvider forced={embed?.theme}>
      <PrefsProvider>
      <SettingsProvider>
      <SpeechProvider>
        <FleetProvider>
          <TooltipProvider>
            <Router hook={useHashLocation}>{embed ? <EmbedScreen params={embed} /> : null}</Router>
            <Toaster position="top-center" />
          </TooltipProvider>
        </FleetProvider>
      </SpeechProvider>
      </SettingsProvider>
      </PrefsProvider>
    </ThemeProvider>
  )
}
