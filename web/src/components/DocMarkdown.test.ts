import { describe, expect, it } from 'vitest'
import { docLinkTarget } from './DocMarkdown'

const base = '/p/acme/chug/files'
const doc = 'docs/design/321-job-groups.md'
const target = (href: string) => docLinkTarget(base, doc, href)

describe('docLinkTarget', () => {
  it('resolves a sibling document against the doc directory', () => {
    expect(target('./309-host-native-execution.md')).toBe(
      `${base}?path=docs%2Fdesign%2F309-host-native-execution.md`,
    )
    expect(target('311-job-inputs.md')).toBe(`${base}?path=docs%2Fdesign%2F311-job-inputs.md`)
  })

  it('walks `..` up to the repo root', () => {
    expect(target('../../spec.md')).toBe(`${base}?path=spec.md`)
    expect(target('../adr/0001-nats.md')).toBe(`${base}?path=docs%2Fadr%2F0001-nats.md`)
  })

  it('treats a leading slash as repo root', () => {
    expect(target('/STYLE.md')).toBe(`${base}?path=STYLE.md`)
  })

  it('carries a heading fragment through to the target document', () => {
    expect(target('./309-host-native-execution.md#decision-4')).toBe(
      `${base}?path=docs%2Fdesign%2F309-host-native-execution.md#decision-4`,
    )
    expect(target('../adr#index')).toBe(`${base}?dir=docs%2Fadr`)
  })

  it('sends extensionless and trailing-slash targets to the directory listing', () => {
    expect(target('../adr/')).toBe(`${base}?dir=docs%2Fadr`)
    expect(target('../../crates')).toBe(`${base}?dir=crates`)
  })

  it('leaves absolute URLs and in-page anchors to the browser', () => {
    expect(target('https://example.com/x.md')).toBeNull()
    expect(target('//example.com/x.md')).toBeNull()
    expect(target('mailto:ops@example.com')).toBeNull()
    expect(target('#decision-7')).toBeNull()
    expect(target('')).toBeNull()
  })

  it('resolves against the repo root for a document at the top level', () => {
    expect(docLinkTarget(base, 'spec.md', './design.md')).toBe(`${base}?path=design.md`)
  })
})
