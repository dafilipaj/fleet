import { CheckIcon, CircleDotIcon, ExternalLinkIcon, LoaderIcon, SendIcon, SparklesIcon, PencilIcon, RotateCcwIcon, Trash2Icon } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'

import type { ReviewEntry, ReviewThread as Thread } from '@/api/types'
import { Markdown } from '@/components/Markdown'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { rangeText } from '@/lib/review'
import { cn } from '@/lib/utils'

const isSubmit = (e: React.KeyboardEvent) => e.key === 'Enter' && (e.metaKey || e.ctrlKey)

/**
 * The box under selected lines: write a comment, then send it now ("Add single comment") or
 * keep it for the review ("Start a review" / "Add review comment"). ⌘↵ does what GitHub does:
 * a review comment once a review is pending, else a single comment.
 */
/** Where a new comment goes: the session, or a note on the PR. */
export type CommentTarget = 'session' | 'note'

export function CommentComposer({
  label,
  drafts,
  onCancel,
  onSubmit,
  onAsk,
  asking = false,
  submitLabel = 'Add single comment',
  target,
}: {
  label: string
  drafts: number
  onCancel: () => void
  onSubmit: (body: string, draft: boolean) => Promise<boolean>
  /** A side question about the lines instead: answered apart from the session's thread. */
  onAsk?: (question: string) => Promise<boolean>
  /** Opened to ask (the selection's "Ask" button): ⌘↵ asks. */
  asking?: boolean
  submitLabel?: string
  /**
   * Where the comment goes, when there is a choice: `session` (feedback the session acts on) or
   * `note` (kept in Fleet on the PR, postable to GitHub). Shown as a toggle above the text.
   */
  target?: { value: CommentTarget; onChange: (t: CommentTarget) => void; note: string }
}) {
  const noting = target?.value === 'note'
  const [body, setBody] = useState('')
  const [busy, setBusy] = useState<false | 'single' | 'draft' | 'ask'>(false)
  const ask = async () => {
    if (!onAsk || !body.trim() || busy) return
    setBusy('ask')
    const ok = await onAsk(body.trim())
    setBusy(false)
    if (ok) onCancel()
  }
  const ref = useRef<HTMLTextAreaElement>(null)
  useEffect(() => ref.current?.focus(), [])
  const go = async (draft: boolean) => {
    if (!body.trim() || busy) return
    setBusy(draft ? 'draft' : 'single')
    const ok = await onSubmit(body, draft)
    setBusy(false)
    if (ok) onCancel()
  }
  return (
    <div className="border-y bg-card px-3 py-2.5 font-sans">
      <div className="mb-1.5 flex flex-wrap items-center gap-2">
        <p className="text-xs text-muted-foreground">{asking && onAsk ? 'Ask about' : 'Comment on'} {label}</p>
        {target && !(asking && onAsk) ? (
          <div role="radiogroup" aria-label="Comment goes to" className="ml-auto flex items-center gap-0.5 rounded-lg bg-muted p-0.5">
            {(
              [
                ['session', 'To the session', 'Feedback the session acts on: sent now, or with the review'],
                ['note', target.note, 'A note kept in Fleet on the PR: post it to GitHub when you want'],
              ] as const
            ).map(([v, text, title]) => (
              <button
                key={v}
                type="button"
                role="radio"
                aria-checked={target.value === v}
                title={title}
                onClick={() => target.onChange(v)}
                className={cn('h-7 rounded-md px-2 text-xs font-medium text-muted-foreground', target.value === v && 'bg-background text-foreground shadow-sm dark:bg-accent')}
              >
                {text}
              </button>
            ))}
          </div>
        ) : null}
      </div>
      <Textarea
        ref={ref}
        value={body}
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          if (isSubmit(e)) {
            e.preventDefault()
            if (asking && onAsk) void ask()
            else void go(!noting && drafts > 0)
          } else if (e.key === 'Escape') {
            e.stopPropagation()
            onCancel()
          }
        }}
        placeholder={asking && onAsk ? 'Ask Claude about these lines — answered on the side, not in the session…' : noting ? 'A note on the PR (kept in Fleet until you post it)…' : 'Ask, suggest a change… (the selected lines go with it)'}
        className="min-h-20 bg-background text-sm"
      />
      <div className="mt-2 flex flex-wrap items-center justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          Cancel
        </Button>
        {onAsk ? (
          <Button
            variant={asking ? 'default' : 'outline'}
            size="sm"
            disabled={!body.trim() || !!busy}
            onClick={() => void ask()}
            title="Answered by a read-only copy of the session: nothing goes into its conversation"
            className="mr-auto"
          >
            {busy === 'ask' ? <LoaderIcon className="animate-spin" /> : <SparklesIcon />}
            Ask on the side
          </Button>
        ) : null}
        {noting ? null : (
          <Button variant={drafts > 0 ? 'default' : 'outline'} size="sm" disabled={!body.trim() || !!busy} onClick={() => void go(true)}>
            {busy === 'draft' ? <LoaderIcon className="animate-spin" /> : null}
            {drafts > 0 ? 'Add review comment' : 'Start a review'}
          </Button>
        )}
        <Button variant={!noting && drafts > 0 ? 'outline' : 'default'} size="sm" disabled={!body.trim() || !!busy} onClick={() => void go(false)}>
          {busy === 'single' ? <LoaderIcon className="animate-spin" /> : null}
          {noting ? 'Add note' : submitLabel}
        </Button>
      </div>
    </div>
  )
}

const STATE_TEXT: Record<ReviewEntry['state'], string> = {
  draft: 'Pending — sent with the review',
  sent: 'Sent to the session…',
  queued: 'Queued — the session is busy',
  working: 'Claude is working…',
  done: '',
}

function Entry({ entry, first, stateText }: { entry: ReviewEntry; first: boolean; stateText?: string }) {
  // Interim narration between tool calls is noise here; the final answer(s) are the reply.
  const finals = entry.replies.filter((r) => r.final)
  const shown = finals.length ? finals : entry.replies.slice(-1)
  return (
    <div className={cn('space-y-2 px-3 py-2', !first && 'border-t')}>
      <div className="flex gap-2">
        <span className="mt-0.5 shrink-0 rounded bg-primary/10 px-1.5 text-[0.6875rem] font-semibold text-primary">You</span>
        <Markdown text={entry.body} className="min-w-0 flex-1 text-sm" />
      </div>
      {shown.map((r, i) => (
        <div key={i} className="flex gap-2">
          <span className="mt-0.5 shrink-0 rounded bg-muted px-1.5 text-[0.6875rem] font-semibold text-muted-foreground">Claude</span>
          <Markdown text={r.text} className={cn('min-w-0 flex-1 text-sm', !r.final && 'text-muted-foreground')} />
        </div>
      ))}
      {entry.state !== 'done' ? (
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          {entry.state === 'draft' ? <CircleDotIcon className="size-3.5 text-status-waiting" /> : <LoaderIcon className="size-3.5 animate-spin" />}
          {stateText ?? STATE_TEXT[entry.state]}
        </p>
      ) : null}
    </div>
  )
}

export interface ThreadActions {
  reply: (thread: string, body: string) => Promise<boolean>
  edit: (thread: string, body: string) => Promise<boolean>
  resolve: (thread: string, resolved: boolean) => Promise<boolean>
  remove: (thread: string) => Promise<boolean>
}

/** A PR note (no session): kept in Fleet until posted to GitHub. */
export interface NoteProps {
  posted: { url: string | null; at: number } | null
  onPost: () => Promise<boolean>
}

/** A comment thread under its lines: the comment, the session's answer(s), follow-ups. `note`: a PR note instead. */
export function ReviewThreadCard({ thread, outdated, actions, note }: { thread: Thread; outdated: boolean; actions: ThreadActions; note?: NoteProps }) {
  const [open, setOpen] = useState(!thread.resolved)
  const [replying, setReplying] = useState(false)
  const [editing, setEditing] = useState(false)
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const draft = thread.status === 'draft' && !note?.posted

  const run = async (fn: () => Promise<boolean>) => {
    setBusy(true)
    const ok = await fn()
    setBusy(false)
    return ok
  }
  const sendReply = async () => {
    if (!text.trim()) return
    if (await run(() => (editing ? actions.edit(thread.id, text) : actions.reply(thread.id, text)))) {
      setText('')
      setReplying(false)
      setEditing(false)
    }
  }

  return (
    <div className="border-y bg-background px-3 py-2 font-sans">
      <div className={cn('overflow-hidden rounded-lg border bg-card', thread.resolved && 'opacity-80')}>
        <div className="flex items-center gap-2 border-b px-3 py-1.5 text-xs">
          <button type="button" className="min-w-0 flex-1 truncate text-left text-muted-foreground" onClick={() => setOpen(!open)} aria-expanded={open}>
            {thread.path.split('/').at(-1)} · {rangeText(thread)}
          </button>
          {note?.posted ? (
            note.posted.url ? (
              <a href={note.posted.url} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 rounded border border-primary/40 px-1.5 text-primary hover:underline">
                On GitHub <ExternalLinkIcon className="size-3" />
              </a>
            ) : (
              <span className="rounded border border-primary/40 px-1.5 text-primary">On GitHub</span>
            )
          ) : note ? (
            <span className="rounded border border-status-waiting/40 px-1.5 text-status-waiting">Note</span>
          ) : draft ? (
            <span className="rounded border border-status-waiting/40 px-1.5 text-status-waiting">Pending</span>
          ) : null}
          {note && !note.posted ? (
            <Button variant="ghost" size="xs" className="h-6 px-1.5 text-xs" title="Post this note as a review comment on the PR, as your gh login" disabled={busy} onClick={() => void run(note.onPost)}>
              {busy ? <LoaderIcon className="size-3.5 animate-spin" /> : <SendIcon className="size-3.5" />}
              Post to GitHub
            </Button>
          ) : null}
          {outdated ? <span className="rounded border px-1.5 text-muted-foreground" title="The lines changed since this comment">Outdated</span> : null}
          {thread.resolved ? <span className="rounded border px-1.5 text-muted-foreground">Resolved</span> : null}
          {draft && note ? (
            <Button
              variant="ghost"
              size="icon"
              className="size-6"
              aria-label={thread.resolved ? 'Reopen' : 'Resolve'}
              title={thread.resolved ? 'Reopen' : 'Resolve (a resolved note is not handed to a session)'}
              onClick={() => void run(() => actions.resolve(thread.id, !thread.resolved))}
            >
              {thread.resolved ? <RotateCcwIcon className="size-3.5" /> : <CheckIcon className="size-3.5" />}
            </Button>
          ) : null}
          {draft ? (
            <Button
              variant="ghost"
              size="icon"
              className="size-6"
              aria-label="Edit"
              title="Edit"
              onClick={() => {
                setEditing(true)
                setReplying(true)
                setText(thread.entries[0]?.body ?? '')
              }}
            >
              <PencilIcon className="size-3.5" />
            </Button>
          ) : (
            <Button
              variant="ghost"
              size="icon"
              className="size-6"
              aria-label={thread.resolved ? 'Reopen' : 'Resolve'}
              title={thread.resolved ? 'Reopen' : 'Resolve'}
              onClick={() => {
                setOpen(thread.resolved)
                void run(() => actions.resolve(thread.id, !thread.resolved))
              }}
            >
              {thread.resolved ? <RotateCcwIcon className="size-3.5" /> : <CheckIcon className="size-3.5" />}
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon"
            className="size-6"
            aria-label={note ? 'Delete note' : draft ? 'Delete draft' : 'Remove from the review'}
            title={note ? (note.posted ? 'Delete the note here (GitHub keeps the comment)' : 'Delete note') : draft ? 'Delete draft' : 'Remove from the review (the conversation keeps it)'}
            onClick={() => void run(() => actions.remove(thread.id))}
          >
            <Trash2Icon className="size-3.5" />
          </Button>
        </div>
        {open ? (
          <>
            {thread.entries.map((e, i) => (
              <Entry key={i} entry={e} first={i === 0} stateText={note ? (note.posted ? 'Posted on GitHub' : 'Only in Fleet — post it, or hand the notes to a session') : undefined} />
            ))}
            <div className="border-t px-3 py-2">
              {replying ? (
                <>
                  <Textarea
                    autoFocus
                    value={text}
                    onChange={(e) => setText(e.target.value)}
                    onKeyDown={(e) => {
                      if (isSubmit(e)) {
                        e.preventDefault()
                        void sendReply()
                      } else if (e.key === 'Escape') {
                        e.stopPropagation()
                        setReplying(false)
                        setEditing(false)
                      }
                    }}
                    placeholder={editing ? 'Edit the comment' : 'Reply — it goes to the session with this thread’s lines'}
                    className="min-h-16 bg-background text-sm"
                  />
                  <div className="mt-2 flex justify-end gap-2">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setReplying(false)
                        setEditing(false)
                      }}
                    >
                      Cancel
                    </Button>
                    <Button size="sm" disabled={!text.trim() || busy} onClick={() => void sendReply()}>
                      {busy ? <LoaderIcon className="animate-spin" /> : null}
                      {editing ? 'Save' : 'Reply'}
                    </Button>
                  </div>
                </>
              ) : draft || note ? null : (
                <button type="button" onClick={() => setReplying(true)} className="w-full rounded-md border bg-background px-2.5 py-1.5 text-left text-sm text-muted-foreground hover:border-ring">
                  Reply…
                </button>
              )}
            </div>
          </>
        ) : null}
      </div>
    </div>
  )
}
