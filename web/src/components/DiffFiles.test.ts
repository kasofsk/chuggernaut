import { describe, expect, it } from 'vitest'
import { diffSectionsByPath, statPathOf } from './DiffFiles'

const diff = [
  'diff --git a/web/src/api.ts b/web/src/api.ts',
  'index 1111111..2222222 100644',
  '--- a/web/src/api.ts',
  '+++ b/web/src/api.ts',
  '@@ -1,3 +1,3 @@',
  ' keep',
  '-- old prose',
  '+++ new prose',
  'diff --git a/old.md b/docs/new.md',
  'similarity index 90%',
  'rename from old.md',
  'rename to docs/new.md',
  '--- a/old.md',
  '+++ b/docs/new.md',
  '@@ -1 +1 @@',
  '-a',
  '+b',
  'diff --git a/gone.txt b/gone.txt',
  'deleted file mode 100644',
  '--- a/gone.txt',
  '+++ /dev/null',
  '@@ -1 +0,0 @@',
  '-bye',
  '',
].join('\n')

describe('diffSectionsByPath', () => {
  const sections = diffSectionsByPath(diff)

  it('keys every section by its new-side path', () => {
    expect([...sections.keys()]).toEqual(['web/src/api.ts', 'docs/new.md', 'gone.txt'])
  })

  it('keeps a section whole and does not bleed into the next file', () => {
    expect(sections.get('web/src/api.ts')).toContain('+++ new prose')
    expect(sections.get('web/src/api.ts')).not.toContain('rename from')
    expect(sections.get('docs/new.md')?.startsWith('diff --git a/old.md b/docs/new.md')).toBe(true)
  })

  it('falls back to the old path for a deletion', () => {
    expect(sections.get('gone.txt')).toContain('-bye')
  })

  it('returns nothing for an empty diff', () => {
    expect(diffSectionsByPath('').size).toBe(0)
  })
})

describe('statPathOf', () => {
  it('passes a plain path through', () => {
    expect(statPathOf('web/src/api.ts')).toBe('web/src/api.ts')
  })

  it('expands both rename notations to the new path', () => {
    expect(statPathOf('docs/{old => new}/f.md')).toBe('docs/new/f.md')
    expect(statPathOf('old.md => docs/new.md')).toBe('docs/new.md')
  })

  it('leaves a braced path with no rename alone', () => {
    expect(statPathOf('web/src/{weird}.ts')).toBe('web/src/{weird}.ts')
  })
})
