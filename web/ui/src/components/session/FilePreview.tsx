import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import {
  ChevronLeftIcon,
  CircleAlertIcon,
  CopyIcon,
  DownloadIcon,
  ExternalLinkIcon,
  FileIcon,
  Loader2Icon,
  XIcon,
} from 'lucide-react'
import { toast } from 'sonner'

import { api, fileRawUrl, isAbortError } from '@/api/client'
import type { FileStat } from '@/api/types'
import { HostBadge } from '@/components/HostBadge'
import { Markdown } from '@/components/Markdown'
import { PlainView } from '@/components/plain/PlainView'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { FileLinksContext, type FileLinkApi } from '@/hooks/useFileLinks'
import { formatBytes } from '@/lib/attach'
import { copyWithToast } from '@/lib/clipboard'
import { relTime } from '@/lib/format'
import { isLocalHref } from '@/lib/markdown'
import { basename, resolveFrom } from '@/lib/paths'
import { isPlainPath, undiffed } from '@/lib/plain'
import { cn } from '@/lib/utils'

/** Same cap as the server (lib/files.mjs PREVIEW_MAX_BYTES): bigger text is download-only. */
const PREVIEW_MAX_BYTES = 5 * 1024 * 1024
/** Rendering more rows than this gets slow; the rest is a download away. */
const MAX_LINES = 20_000

export interface FilePreviewProps {
  host: string
  id: string
  /** The file to show (from a chat link); null = closed. */
  file: FileStat | null
  onClose: () => void
  /** Desktop: a large centred dialog. Mobile (default): a full-screen sheet. */
  desktop?: boolean
}

type Loaded = { path: string; text: string } | { path: string; error: string } | null

/**
 * The in-app file preview: header (name, path, host, size · age, Download / Open on host /
 * Copy path / Close) over the content by kind — markdown rendered (links to other local
 * files open here, relative images load from the host), text with line numbers (the
 * mention's `:line` highlighted and scrolled to), images, PDFs, else an info panel.
 */
export function FilePreview({ host, id, file, onClose, desktop = false }: FilePreviewProps) {
  // Links inside a previewed file push onto this stack; Back pops. It belongs to `file`: a
  // new file from the chat starts a new stack.
  const [nav, setNav] = useState<{ root: FileStat | null; stack: FileStat[] }>({ root: null, stack: [] })
  const stack = nav.root === file && nav.stack.length ? nav.stack : file ? [file] : []
  const cur = stack[stack.length - 1] ?? null
  // Markdown: rendered, or its source (the default when the mention points at a line).
  const [sourceFor, setSourceFor] = useState<{ f: FileStat; on: boolean } | null>(null)
  const textual = cur?.kind === 'markdown' || cur?.kind === 'text'
  // Rendered by default: markdown, and PlainLang specs (served as text).
  const plain = !!cur && textual && isPlainPath(cur.path)
  const renders = cur?.kind === 'markdown' || plain
  const source = cur ? (sourceFor?.f === cur ? sourceFor.on : !!cur.line && renders) : false
  const toggleSource = () => cur && setSourceFor({ f: cur, on: !source })

  const tooBig = textual && (cur?.size ?? 0) > PREVIEW_MAX_BYTES
  const [loaded, setLoaded] = useState<Loaded>(null)
  useEffect(() => {
    if (!cur || !textual || tooBig) return
    const ctl = new AbortController()
    api
      .fileText(host, id, cur.path, { signal: ctl.signal })
      .then((text) => setLoaded({ path: cur.path, text }))
      .catch((err) => {
        if (!isAbortError(err)) setLoaded({ path: cur.path, error: (err as Error)?.message || 'could not load the file' })
      })
    return () => ctl.abort()
  }, [host, id, cur, textual, tooBig])

  // Inside the preview: `[t](rel)` links open here (stat first); relative images load from the host.
  const previewLinks = useMemo<FileLinkApi | null>(() => {
    if (!cur || !file) return null
    const go = async (href: string) => {
      const ref = resolveFrom(cur.path, href)
      if (!ref) return
      try {
        const r = await api.fileStat(host, id, [ref.path])
        const f = r.files[0]
        if (!f?.exists) toast.error(f?.forbidden ? `Not viewable: ${ref.path}` : `Not found: ${ref.path}`)
        else if (!f.isFile) toast(`${f.rel ?? ref.path} is a folder`)
        else {
          const next = { ...f, ...(ref.line ? { line: ref.line } : {}) }
          setNav((n) => ({ root: file, stack: [...(n.root === file && n.stack.length ? n.stack : [file]), next] }))
        }
      } catch (err) {
        toast.error((err as Error)?.message || 'could not open the link')
      }
    }
    return {
      isLink: (_raw, src) => src === 'file',
      open: (raw) => void go(raw),
      imageSrc: (src) => {
        if (!isLocalHref(src)) return null
        const ref = resolveFrom(cur.path, src)
        return ref ? fileRawUrl(host, id, ref.path) : null
      },
    }
  }, [cur, file, host, id])

  const openOnHost = async () => {
    if (!cur) return
    try {
      const r = await api.fileOpen(host, id, cur.path)
      toast.success(r.revealed ? `Shown in its folder on ${host}` : `Opened on ${host}`, { description: cur.rel ?? cur.path })
    } catch (err) {
      toast.error(`Could not open on ${host}`, { description: (err as Error)?.message })
    }
  }

  const name = cur ? basename(cur.path) : ''
  const rawUrl = cur ? fileRawUrl(host, id, cur.path) : ''
  const text = loaded && cur && loaded.path === cur.path && 'text' in loaded ? loaded.text : null
  const error = loaded && cur && loaded.path === cur.path && 'error' in loaded ? loaded.error : null

  let body: React.ReactNode = null
  if (cur) {
    if (tooBig || cur.kind === 'other' || !cur.kind) {
      body = (
        <Info
          text={tooBig ? `Too large to preview (${formatBytes(cur.size ?? 0)})` : 'No preview for this kind of file'}
          host={host}
          downloadUrl={fileRawUrl(host, id, cur.path, { download: true })}
          name={name}
          onOpen={openOnHost}
        />
      )
    } else if (cur.kind === 'image') {
      body = (
        <div className="flex min-h-full items-center justify-center p-4">
          <img src={rawUrl} alt={name} className="max-h-full max-w-full rounded-md object-contain" />
        </div>
      )
    } else if (cur.kind === 'pdf') {
      body = <iframe src={rawUrl} title={name} className="block size-full border-0 bg-white" />
    } else if (error) {
      body = <Info text={error} host={host} downloadUrl={fileRawUrl(host, id, cur.path, { download: true })} name={name} onOpen={openOnHost} />
    } else if (text === null) {
      body = (
        <div className="flex items-center justify-center gap-2 py-16 text-sm text-dimmer">
          <Loader2Icon className="size-4 animate-spin" /> Loading…
        </div>
      )
    } else if (plain && !source) {
      body = (
        <div className={cn('mx-auto w-full px-4 py-4', desktop ? 'max-w-6xl pl-6' : 'max-w-3xl')}>
          <PlainPreview text={text} name={basename(cur.path).replace(/\.plain$/i, '')} onOpenModule={previewLinks ? (m) => previewLinks.open(`./${m}.plain`, 'file') : undefined} />
        </div>
      )
    } else if (cur.kind === 'markdown' && !source) {
      body = (
        <div className={cn('mx-auto w-full px-4 py-4', desktop ? 'max-w-4xl' : 'max-w-3xl')} style={{ fontSize: 15, lineHeight: 1.55 }}>
          <FileLinksContext.Provider value={previewLinks}>
            <Markdown text={text} />
          </FileLinksContext.Provider>
        </div>
      )
    } else {
      body = <CodeLines text={text} line={cur.line} />
    }
  }

  const meta = cur ? [cur.size != null ? formatBytes(cur.size) : '', cur.mtime ? `modified ${relTime(cur.mtime)} ago` : ''].filter(Boolean).join(' · ') : ''

  return (
    <Dialog open={!!file} onOpenChange={(o) => (o ? null : onClose())}>
      <DialogContent
        showCloseButton={false}
        className={cn(
          'flex flex-col gap-0 overflow-hidden p-0 sm:max-w-none',
          desktop
            ? 'h-[88vh] w-[min(96vw,1100px)] max-w-none'
            : 'top-0 left-0 h-app w-full max-w-none translate-x-0 translate-y-0 rounded-none pt-safe pb-safe ring-0',
        )}
      >
        <header className="shrink-0 border-b px-safe">
          <div className="px-3 py-2">
            <div className="flex items-center gap-1.5">
              {stack.length > 1 ? (
                <Button variant="ghost" size="icon" aria-label="Back" className={desktop ? '' : 'size-11'} onClick={() => setNav({ root: file, stack: stack.slice(0, -1) })}>
                  <ChevronLeftIcon className="size-5" />
                </Button>
              ) : (
                <FileIcon className="ml-1 size-4 shrink-0 text-dimmer" />
              )}
              <div className="min-w-0 flex-1 pl-1">
                <DialogTitle className="truncate text-[0.9375rem] leading-tight font-semibold">{name}</DialogTitle>
                <DialogDescription className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                  <span className="truncate font-mono" title={cur?.path}>
                    {cur?.rel ?? cur?.path}
                    {cur?.line ? `:${cur.line}` : ''}
                  </span>
                </DialogDescription>
              </div>
              <Button variant="ghost" size="icon" aria-label="Close" className={desktop ? '' : 'size-11'} onClick={onClose}>
                <XIcon className="size-5" />
              </Button>
            </div>
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5 pl-1">
              <HostBadge host={host} />
              {meta ? <span className="text-xs text-dimmer tabular-nums">{meta}</span> : null}
              <span className="flex-1" />
              {renders && !tooBig ? (
                <Button variant="outline" size="sm" onClick={toggleSource} aria-pressed={source}>
                  {source ? 'Rendered' : 'Source'}
                </Button>
              ) : null}
              <Button variant="outline" size="sm" asChild>
                <a href={cur ? fileRawUrl(host, id, cur.path, { download: true }) : undefined} download={name}>
                  <DownloadIcon /> Download
                </a>
              </Button>
              <Button variant="outline" size="sm" onClick={openOnHost} title={`Opens the file with its default app on ${host} — not in this browser`}>
                <ExternalLinkIcon /> Open on {host}
              </Button>
              <Button variant="outline" size="sm" onClick={() => cur && void copyWithToast(cur.path)}>
                <CopyIcon /> Copy path
              </Button>
            </div>
          </div>
        </header>
        <div className={cn('min-h-0 flex-1 overscroll-contain', cur?.kind === 'pdf' ? 'overflow-hidden' : 'overflow-auto')}>{body}</div>
      </DialogContent>
    </Dialog>
  )
}

function PlainPreview({ text, name, onOpenModule }: { text: string; name: string; onOpenModule?: (m: string) => void }) {
  const diff = useMemo(() => undiffed(text), [text])
  return <PlainView diff={diff} name={name} diffed={false} index onOpenModule={onOpenModule} />
}

function Info({ text, host, downloadUrl, name, onOpen }: { text: string; host: string; downloadUrl: string; name: string; onOpen: () => void }) {
  return (
    <div className="flex flex-col items-center gap-3 px-4 py-16 text-center text-sm text-muted-foreground">
      <CircleAlertIcon className="size-6 text-dimmer" />
      <span className="max-w-md break-words">{text}</span>
      <div className="flex flex-wrap justify-center gap-2">
        <Button variant="outline" asChild>
          <a href={downloadUrl} download={name}>
            <DownloadIcon /> Download
          </a>
        </Button>
        <Button variant="outline" onClick={onOpen}>
          <ExternalLinkIcon /> Open on {host}
        </Button>
      </div>
    </div>
  )
}

/** Monospace text with line numbers; `line` is highlighted and scrolled into view. */
export function CodeLines({ text, line }: { text: string; line?: number }) {
  const ref = useRef<HTMLDivElement>(null)
  const lines = useMemo(() => text.replace(/\r\n?/g, '\n').replace(/\n$/, '').split('\n'), [text])
  const shown = lines.length > MAX_LINES ? lines.slice(0, MAX_LINES) : lines
  useLayoutEffect(() => {
    if (!line) return
    ref.current?.querySelector(`[data-line="${line}"]`)?.scrollIntoView({ block: 'center' })
  }, [line, text])
  const width = String(shown.length).length
  return (
    <div ref={ref} className="min-w-max py-2 font-mono text-[0.78125rem] leading-5">
      {shown.map((l, i) => (
        <div key={i} data-line={i + 1} className={cn('flex', i + 1 === line && 'bg-status-waiting/15')}>
          <span
            className={cn('sticky left-0 shrink-0 bg-popover pr-3 pl-3 text-right text-dimmer select-none', i + 1 === line && 'text-status-waiting')}
            style={{ width: `${width + 3}ch` }}
          >
            {i + 1}
          </span>
          <span className="pr-4 whitespace-pre">{l || ' '}</span>
        </div>
      ))}
      {lines.length > MAX_LINES ? (
        <div className="px-3 py-2 text-xs text-dimmer">… {lines.length - MAX_LINES} more lines — download the file to see them all</div>
      ) : null}
    </div>
  )
}
