
import { describe, expect, it } from 'vitest'
import { ApiError, type Input } from '../api'
import { inputFieldErrors, inputValueError, inputsOrUndefined, suppliedInputs } from './JobInputs'

const declare = (over: Partial<Input>): Input => ({
  name: 'sha',
  type: 'string',
  required: false,
  ...over,
})

describe('inputValueError', () => {
  it('accepts the shapes the charset exists to allow', () => {
    for (const value of ['4f9c1ab', 'ghcr.io/org/img:sha', 'img@sha256:abc', 'feature/x', 'a+b-c_d.e'])
      expect(inputValueError(declare({}), value)).toBeNull()
  })

  it('refuses a value outside the default charset', () => {
    for (const value of ['a b', 'a;b', 'a$b', 'a`b', "a'b", 'a\nb', 'a#b', 'héllo'])
      expect(inputValueError(declare({}), value)).toMatch(/identifiers/)
  })

  it('refuses a value over the 256-character bound, naming the length', () => {
    expect(inputValueError(declare({}), 'a'.repeat(256))).toBeNull()
    expect(inputValueError(declare({}), 'a'.repeat(257))).toBe(
      '257 characters, over the 256-character limit',
    )
  })

  it('applies a declared pattern as a WHOLE-value match', () => {
    const sha = declare({ pattern: '[0-9a-f]{7,40}' })
    expect(inputValueError(sha, '4f9c1ab')).toBeNull()
    expect(inputValueError(sha, 'zzz')).toMatch(/does not match/)
    expect(inputValueError(sha, '4f9c1abZZ')).toMatch(/does not match/)
  })

  it('applies a declared pattern only ON TOP of the charset, never instead of it', () => {
    expect(inputValueError(declare({ pattern: '.*' }), 'rm -rf /')).toMatch(/identifiers/)
  })

  it('skips a pattern JavaScript cannot compile rather than inventing a verdict', () => {
    expect(inputValueError(declare({ pattern: '(?<' }), 'anything')).toBeNull()
  })

  it('holds an enum to its declared values', () => {
    const service = declare({ type: 'enum', values: ['web', 'worker'] })
    expect(inputValueError(service, 'web')).toBeNull()
    expect(inputValueError(service, 'bot')).toBe('not one of web, worker')
  })

  it('treats an unknown kind as text under the charset alone (spec §14 N+1)', () => {
    const future = declare({ type: 'gradient' as Input['type'], values: ['a'] })
    expect(inputValueError(future, 'anything.here')).toBeNull()
    expect(inputValueError(future, 'a b')).toMatch(/identifiers/)
  })

  it('never flags an empty value, including a required one', () => {
    expect(inputValueError(declare({ required: true, pattern: '[0-9a-f]{7}' }), '')).toBeNull()
  })
})

describe('suppliedInputs', () => {
  const declared = [declare({ name: 'sha' }), declare({ name: 'service' })]

  it('is undefined when nothing is supplied, so an input-less type sends today’s body', () => {
    expect(suppliedInputs([], {})).toBeUndefined()
    expect(suppliedInputs(declared, {})).toBeUndefined()
    expect(suppliedInputs(declared, { sha: '', service: '   ' })).toBeUndefined()
  })

  it('sends supplied values trimmed, in declaration order', () => {
    expect(Object.entries(suppliedInputs(declared, { service: 'web', sha: ' 4f9c1ab ' }) ?? {})).toEqual([
      ['sha', '4f9c1ab'],
      ['service', 'web'],
    ])
  })

  it('drops a value the selected type does not declare', () => {
    expect(suppliedInputs(declared, { sha: '4f9c1ab', region: 'eu' })).toEqual({ sha: '4f9c1ab' })
  })

  it('inputsOrUndefined keeps the whole map — the Draft patch before its declaration loads', () => {
    expect(inputsOrUndefined({ sha: '4f9c1ab', region: 'eu' })).toEqual({ sha: '4f9c1ab', region: 'eu' })
    expect(inputsOrUndefined({})).toBeUndefined()
  })
})

describe('inputFieldErrors', () => {
  it('reads the creation pass’s single message', () => {
    const err = new ApiError(422, { error: "inputs: input 'sha': value is empty" })
    expect(inputFieldErrors(err)).toEqual({ sha: 'value is empty' })
  })

  it('reads the release envelope, keyed inputs.{name}', () => {
    const err = new ApiError(422, {
      errors: [
        { field: 'inputs.sha', message: 'required input has no value' },
        { field: 'timeout', message: 'unparseable' },
      ],
    })
    expect(inputFieldErrors(err)).toEqual({ sha: 'required input has no value' })
  })

  it('maps nothing it cannot key, leaving the caller’s banner to carry it', () => {
    expect(inputFieldErrors(new ApiError(422, { error: 'cover_html too large' }))).toEqual({})
    expect(inputFieldErrors(new ApiError(500, null))).toEqual({})
    expect(inputFieldErrors(new Error('offline'))).toEqual({})
  })
})
