import { useState } from 'react'
import { CheckIcon, CircleHelpIcon, Loader2Icon, SendHorizontalIcon, XIcon } from 'lucide-react'

import type { Message, Question, QuestionAnswer } from '@/api/types'
import { Button } from '@/components/ui/button'
import { emptyDrafts, isAnswered, splitRecommended, toAnswers, toggleOption, typeOther, type QuestionDraft } from '@/lib/questions'
import { cn } from '@/lib/utils'

export interface QuestionActions {
  /** Types the answers into the terminal; rejects with the reason when nothing was typed. */
  answer: (toolUseId: string, answers: QuestionAnswer[]) => Promise<void>
  /** Esc: dismiss the prompt, then reply in the composer. */
  dismiss: () => Promise<void>
}

/** Claude's AskUserQuestion prompt: answerable while it is the one Claude waits on, a record after. */
export function QuestionCard({ m, actions }: { m: Message; actions?: QuestionActions | null }) {
  const questions = m.questions ?? []
  const open = !m.answers && !m.declined
  return (
    <div className="flex w-full flex-col items-start">
      <div
        className={cn(
          'flex w-full flex-col gap-3 rounded-2xl rounded-bl-md border bg-card px-3 py-2.5 text-card-foreground',
          open && actions ? 'border-status-waiting/60 shadow-[0_0_0_3px] shadow-status-waiting/10' : 'border-border/70',
        )}
      >
        {open && actions ? <AnswerForm key={m.id} id={m.id ?? ''} questions={questions} actions={actions} /> : <Record m={m} questions={questions} />}
      </div>
    </div>
  )
}

function Heading({ q }: { q: Question }) {
  return (
    <div className="flex flex-col gap-1">
      {q.header ? (
        <span className="inline-flex w-fit items-center gap-1 rounded-full bg-status-waiting/15 px-2 py-0.5 text-[0.6875rem] font-semibold text-status-waiting">
          <CircleHelpIcon className="size-3" />
          {q.header}
        </span>
      ) : null}
      <div className="font-medium whitespace-pre-wrap [overflow-wrap:anywhere]">{q.question}</div>
    </div>
  )
}

/** Answered or dismissed: each question with what was answered. */
function Record({ m, questions }: { m: Message; questions: Question[] }) {
  return (
    <>
      {questions.map((q, i) => (
        <div key={i} className="flex flex-col gap-1.5">
          <Heading q={q} />
          {m.answers ? (
            <div className="flex items-start gap-1.5 text-[0.92em] text-muted-foreground">
              <CheckIcon className="mt-[0.2em] size-3.5 shrink-0 text-primary" />
              <span className="[overflow-wrap:anywhere]">{m.answers[i] ?? '—'}</span>
            </div>
          ) : (
            <OptionList q={q} />
          )}
        </div>
      ))}
      {m.declined ? <div className="text-[0.8em] text-dimmer">Dismissed — answered in the chat</div> : null}
      {!m.answers && !m.declined ? <div className="text-[0.8em] text-dimmer">Waiting for an answer</div> : null}
    </>
  )
}

/** The options, read-only. */
function OptionList({ q }: { q: Question }) {
  return (
    <ul className="flex flex-col gap-0.5 text-[0.92em] text-muted-foreground">
      {q.options.map((o, i) => (
        <li key={i} className="[overflow-wrap:anywhere]">
          {i + 1}. {o.label}
        </li>
      ))}
    </ul>
  )
}

function AnswerForm({ id, questions, actions }: { id: string; questions: Question[]; actions: QuestionActions }) {
  const [drafts, setDrafts] = useState<QuestionDraft[]>(() => emptyDrafts(questions))
  const [busy, setBusy] = useState<'answer' | 'dismiss' | null>(null)
  const set = (i: number, d: QuestionDraft) => setDrafts((all) => all.map((x, j) => (j === i ? d : x)))
  const ready = drafts.every(isAnswered)

  const run = async (kind: 'answer' | 'dismiss') => {
    if (busy) return
    setBusy(kind)
    try {
      await (kind === 'answer' ? actions.answer(id, toAnswers(drafts)) : actions.dismiss())
      // Stays busy: the card turns into the record once the transcript has the answer.
    } catch {
      setBusy(null)
    }
  }

  return (
    <>
      {questions.map((q, i) => (
        <fieldset key={i} className="flex min-w-0 flex-col gap-2" disabled={!!busy}>
          <Heading q={q} />
          {q.multiSelect ? <div className="-mt-1 text-[0.75em] text-dimmer">Pick any</div> : null}
          <div className="flex flex-col gap-1.5">
            {q.options.map((o, j) => {
              const on = drafts[i].options.includes(j)
              const { label, recommended } = splitRecommended(o.label)
              return (
                <button
                  key={j}
                  type="button"
                  role={q.multiSelect ? 'checkbox' : 'radio'}
                  aria-checked={on}
                  onClick={() => set(i, toggleOption(q, drafts[i], j))}
                  className={cn(
                    'flex min-h-11 w-full items-start gap-2.5 rounded-xl border px-3 py-2 text-left transition-colors',
                    on ? 'border-primary bg-accent text-accent-foreground' : 'border-border bg-background/40 hover:bg-muted',
                  )}
                >
                  <span
                    aria-hidden
                    className={cn(
                      'mt-[0.2em] flex size-4 shrink-0 items-center justify-center border',
                      q.multiSelect ? 'rounded-[4px]' : 'rounded-full',
                      on ? 'border-primary bg-primary text-primary-foreground' : 'border-muted-foreground/50',
                    )}
                  >
                    {on ? <CheckIcon className="size-3" strokeWidth={3} /> : null}
                  </span>
                  <span className="flex min-w-0 flex-col">
                    <span className="font-medium [overflow-wrap:anywhere]">
                      {label}
                      {recommended ? <span className="ml-1.5 text-[0.75em] font-normal text-status-waiting">Recommended</span> : null}
                    </span>
                    {o.description ? <span className="text-[0.85em] text-muted-foreground [overflow-wrap:anywhere]">{o.description}</span> : null}
                  </span>
                </button>
              )
            })}
            {q.noOther ? null : (
              <input
                value={drafts[i].text}
                onChange={(e) => set(i, typeOther(q, drafts[i], e.target.value))}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.nativeEvent.isComposing && ready) {
                    e.preventDefault()
                    void run('answer')
                  }
                }}
                placeholder="Something else…"
                aria-label={`Other answer: ${q.question}`}
                className={cn(
                  'min-h-11 w-full rounded-xl border bg-background/40 px-3 py-2 text-base outline-none placeholder:text-dimmer focus-visible:border-ring md:text-[0.95em]',
                  drafts[i].text.trim() ? 'border-primary' : 'border-border',
                )}
              />
            )}
          </div>
        </fieldset>
      ))}
      <div className="flex items-center justify-end gap-2 pt-0.5">
        <Button variant="ghost" size="sm" className="h-9 rounded-full text-muted-foreground" disabled={!!busy} onClick={() => run('dismiss')}>
          {busy === 'dismiss' ? <Loader2Icon className="animate-spin" /> : <XIcon />}
          Dismiss
        </Button>
        <Button size="sm" className="h-9 rounded-full px-4" disabled={!ready || !!busy} onClick={() => run('answer')}>
          {busy === 'answer' ? <Loader2Icon className="animate-spin" /> : <SendHorizontalIcon />}
          {questions.length > 1 ? 'Submit answers' : 'Answer'}
        </Button>
      </div>
    </>
  )
}
