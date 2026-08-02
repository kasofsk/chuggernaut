import { describe, expect, it } from 'vitest'
import { dayLabelIndexes } from './ActivityChart'

const LABEL_PX = 54

describe('dayLabelIndexes', () => {
  it('labels first and last when there is room for both', () => {
    expect(dayLabelIndexes(14, 566, LABEL_PX)).toEqual([0, 2, 4, 6, 8, 10, 13])
  })

  it('thins the labels as the plot narrows', () => {
    expect(dayLabelIndexes(14, 296, LABEL_PX)).toEqual([0, 3, 6, 9, 13])
    expect(dayLabelIndexes(14, 140, LABEL_PX)).toEqual([0, 6, 13])
  })

  it('drops the last day rather than colliding with the first', () => {
    expect(dayLabelIndexes(14, 40, LABEL_PX)).toEqual([0])
  })

  it('never places two labels closer than one label width, at any width', () => {
    for (let n = 1; n <= 31; n++) {
      for (let plotW = 40; plotW <= 900; plotW += 7) {
        const idx = dayLabelIndexes(n, plotW, LABEL_PX)
        expect(idx[0]).toBe(0)
        expect(idx.every((i) => i >= 0 && i < n)).toBe(true)
        const colW = plotW / n
        for (let k = 1; k < idx.length; k++) {
          expect((idx[k] - idx[k - 1]) * colW).toBeGreaterThanOrEqual(Math.min(LABEL_PX, colW))
        }
      }
    }
  })

  it('handles an empty series', () => {
    expect(dayLabelIndexes(0, 500, LABEL_PX)).toEqual([])
  })
})
