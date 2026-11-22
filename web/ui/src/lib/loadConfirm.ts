// The confirm a spawn on a host under heavy load asks for (web/lib/load.mjs): `request()` calls
// `confirmLoad` on a 409 `overloaded`, LoadConfirmHost answers it with a dialog. No host mounted
// (a test, a page without it): no — the request fails with the server's message.
type Asker = (message: string) => Promise<boolean>

let asker: Asker | null = null

export function setLoadConfirm(fn: Asker | null): void {
  asker = fn
}

export function confirmLoad(message: string): Promise<boolean> {
  return asker ? asker(message) : Promise.resolve(false)
}
