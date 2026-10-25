import type { ReactNode } from 'react'
import { useLocation } from 'wouter'
import { AudioLinesIcon, InboxIcon, BotIcon, CircleDotIcon, GaugeIcon, GitPullRequestIcon, NotebookPenIcon, SquareCheckBigIcon, NotebookTextIcon, Settings2Icon } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { DropdownMenuItem } from '@/components/ui/dropdown-menu'
import { useNotesHosts } from '@/hooks/useNotes'
import { useSettings } from '@/hooks/useSettings'
import { scratchpad } from '@/lib/scratchpad'
import { withHint, type ShortcutAction } from '@/lib/shortcuts'
import { cn } from '@/lib/utils'

export type ModuleId = 'inbox' | 'scratchpad' | 'memos' | 'operator' | 'prs' | 'tasks' | 'notes' | 'jobs' | 'usage' | 'settings'

export interface Module {
  id: ModuleId
  label: string
  icon: ReactNode
  /** A screen (`#/…`), or an action (the scratchpad panel). */
  href?: string
  run?: () => void
  hint?: ShortcutAction
  /** `work`: the modules you work in; `utility`: the helper screens (usage, settings). */
  group: 'work' | 'utility'
}

/**
 * The app's modules — the screens and panels next to the session list — in one order, for every
 * place that lists them: the desktop sidebar's module row, the collapsed rail, the mobile ⋯ menu.
 * Modules this server doesn't offer (no notes, no operator) are left out.
 */
function useModules(): Module[] {
  const notesHosts = useNotesHosts()
  const settings = useSettings()
  const all: (Module | null)[] = [
    { id: 'inbox', label: 'Inbox', icon: <InboxIcon />, href: '#/inbox', hint: 'inbox', group: 'work' },
    settings.operator ? { id: 'operator', label: 'Operator', icon: <AudioLinesIcon />, href: '#/operator', hint: 'operator', group: 'work' } : null,
    { id: 'prs', label: 'Pull requests', icon: <GitPullRequestIcon />, href: '#/prs', hint: 'prs', group: 'work' },
    { id: 'tasks', label: 'Tasks', icon: <SquareCheckBigIcon />, href: '#/tasks', hint: 'tasks', group: 'work' },
    settings.memos ? { id: 'memos', label: 'Voice memos', icon: <CircleDotIcon />, href: '#/memos', hint: 'memos', group: 'work' } : null,
    { id: 'scratchpad', label: 'Scratchpad', icon: <NotebookPenIcon />, run: () => (scratchpad.isOpen() ? scratchpad.hide() : scratchpad.show()), hint: 'scratchpad', group: 'work' },
    notesHosts.length ? { id: 'notes', label: 'Notes', icon: <NotebookTextIcon />, href: '#/notes', hint: 'notes', group: 'work' } : null,
    { id: 'jobs', label: 'Bots', icon: <BotIcon />, href: '#/bots', hint: 'jobs', group: 'work' },
    { id: 'usage', label: 'Usage limits', icon: <GaugeIcon />, href: '#/usage', hint: 'usage', group: 'utility' },
    { id: 'settings', label: 'Settings', icon: <Settings2Icon />, href: '#/settings', hint: 'settings', group: 'utility' },
  ]
  return all.filter((m): m is Module => m !== null)
}

/** One icon button per module (the top bar); the open one is highlighted. The scratchpad has none there: ⌘K or its shortcut. */
export function ModuleButtons({
  size = 'icon',
  className,
  exclude = [],
  group,
}: {
  size?: 'icon' | 'icon-sm'
  className?: string
  exclude?: ModuleId[]
  /** Only this group (absent: all). */
  group?: Module['group']
}) {
  const [location] = useLocation()
  const modules = useModules().filter((m) => m.id !== 'scratchpad' && !exclude.includes(m.id) && (!group || m.group === group))
  return (
    <>
      {modules.map((m) => {
        const title = m.hint ? withHint(m.label, m.hint) : m.label
        const path = m.href?.slice(1)
        const active = !!path && (location === path || location.startsWith(`${path}/`))
        const cls = cn('text-dimmer hover:text-foreground', className, active && 'bg-accent text-accent-foreground')
        return m.href ? (
          <Button key={m.id} asChild variant="ghost" size={size} className={cls}>
            <a href={m.href} aria-label={m.label} aria-current={active ? 'page' : undefined} title={title}>
              {m.icon}
            </a>
          </Button>
        ) : (
          <Button key={m.id} variant="ghost" size={size} aria-label={m.label} title={title} onClick={m.run} className={cls}>
            {m.icon}
          </Button>
        )
      })}
    </>
  )
}

/** Finger-sized rows for the mobile ⋯ menu. */
export const MENU_ITEM = 'min-h-11 gap-3 px-3 text-[0.9375rem] [&_svg:not([class*=size-])]:size-5'

/** The modules as dropdown items (the mobile ⋯ menu); `group` picks one group. */
export function ModuleMenuItems({ exclude = [], group }: { exclude?: ModuleId[]; group?: Module['group'] }) {
  const modules = useModules().filter((m) => !exclude.includes(m.id) && (!group || m.group === group))
  return (
    <>
      {modules.map((m) =>
        m.href ? (
          <DropdownMenuItem key={m.id} asChild className={MENU_ITEM}>
            <a href={m.href}>
              {m.icon}
              {m.label}
            </a>
          </DropdownMenuItem>
        ) : (
          <DropdownMenuItem key={m.id} onSelect={m.run} className={MENU_ITEM}>
            {m.icon}
            {m.label}
          </DropdownMenuItem>
        ),
      )}
    </>
  )
}
