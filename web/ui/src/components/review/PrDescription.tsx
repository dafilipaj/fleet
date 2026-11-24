import { ChevronDownIcon, ChevronRightIcon, FileTextIcon } from 'lucide-react'

import { Markdown } from '@/components/Markdown'
import { descriptionAnchor, descriptionExcerpt } from '@/lib/review'
import { cn } from '@/lib/utils'

/** The PR's description (title, author, body as markdown) above its files; folds to one line. */
export function PrDescription({ entryKey, title, author, body, open, onToggle }: { entryKey: string; title?: string; author?: string | null; body: string; open: boolean; onToggle: () => void }) {
  const text = body.trim()
  return (
    <section id={descriptionAnchor(entryKey)} aria-label="Pull request description" className="scroll-mt-2 overflow-hidden rounded-lg border bg-card">
      <button type="button" aria-expanded={open} onClick={onToggle} className="flex w-full min-w-0 items-center gap-1.5 px-3 py-2 text-left text-sm hover:bg-muted/50">
        {open ? <ChevronDownIcon className="size-4 shrink-0 text-muted-foreground" /> : <ChevronRightIcon className="size-4 shrink-0 text-muted-foreground" />}
        <FileTextIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="shrink-0 font-semibold">Description</span>
        {title ? <span className="truncate text-muted-foreground">· {title}</span> : null}
        {!open && !title && text ? <span className="min-w-0 truncate text-muted-foreground">{descriptionExcerpt(text)}</span> : null}
        {author ? <span className="ml-auto shrink-0 pl-2 text-xs text-dimmer">by {author}</span> : null}
      </button>
      {open ? (
        <div className={cn('border-t px-4 py-3', !text && 'text-sm text-muted-foreground italic')}>
          {text ? <Markdown text={text} className="min-w-0 text-sm" /> : 'No description provided.'}
        </div>
      ) : null}
    </section>
  )
}

/** At the top of the file tree: the PR's title and the start of its description; click → the description. */
export function PrDescriptionRow({ title, number, body, onOpen }: { title: string; number?: number; body: string; onOpen: () => void }) {
  const excerpt = descriptionExcerpt(body)
  return (
    <button type="button" onClick={onOpen} title="Show the description" className="mt-2 block w-full rounded-lg border bg-card/40 px-2 py-1.5 text-left hover:bg-muted">
      <span className="flex min-w-0 items-start gap-1.5 text-xs font-semibold">
        <FileTextIcon className="mt-px size-3.5 shrink-0 text-muted-foreground" />
        <span className="line-clamp-2">
          {title || 'Pull request'}
          {number ? <span className="font-normal text-muted-foreground"> #{number}</span> : null}
        </span>
      </span>
      <span className={cn('mt-1 line-clamp-3 text-[0.6875rem] leading-snug', excerpt ? 'text-muted-foreground' : 'text-dimmer italic')}>{excerpt || 'No description provided.'}</span>
    </button>
  )
}
