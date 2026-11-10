import test from 'node:test';
import assert from 'node:assert/strict';
import {
  REVIEW_INTERVAL_MS,
  REVIEW_MIN_AGE_MS,
  buildMapUrl,
  markRecommended,
  parseReviewState,
  selectReviewCandidate,
  upsertInstall,
} from '../src/shared/utils/reviewPrompt.ts';
import * as reviewPrompt from '../src/shared/utils/reviewPrompt.ts';

const NOW = Date.UTC(2026, 10, 9, 12, 0, 0);
const HOUR = 60 * 60 * 1000;
const DAY = 24 * HOUR;

function map(mapId, ageMs, overrides = {}) {
  return {
    mapId,
    title: `Карта ${mapId}`,
    installedAt: NOW - ageMs,
    ...overrides,
  };
}

function state(installs, overrides = {}) {
  return { installs, recommended: [], lastShownAt: 0, ...overrides };
}

test('карта младше суток не рекомендуется', () => {
  assert.equal(selectReviewCandidate(state([map(1, REVIEW_MIN_AGE_MS - 1)]), NOW), null);
});

test('ровно через сутки карта становится кандидатом', () => {
  const candidate = selectReviewCandidate(state([map(1, REVIEW_MIN_AGE_MS)]), NOW);
  assert.equal(candidate?.mapId, 1);
});

test('очень старая карта всё равно рекомендуется (лимита 14 дней нет)', () => {
  const candidate = selectReviewCandidate(state([map(1, 60 * DAY)]), NOW);
  assert.equal(candidate?.mapId, 1);
});

test('из нескольких карт выбирается самая давняя установка', () => {
  const candidate = selectReviewCandidate(
    state([map(1, 2 * DAY), map(2, 10 * DAY), map(3, 3 * DAY)]),
    NOW,
  );
  assert.equal(candidate?.mapId, 2);
});

test('уже рекомендованная карта пропускается', () => {
  const candidate = selectReviewCandidate(
    state([map(1, 10 * DAY), map(2, 2 * DAY)], { recommended: [1] }),
    NOW,
  );
  assert.equal(candidate?.mapId, 2);
});

test('внутри 12 часов рекомендация не повторяется', () => {
  const candidate = selectReviewCandidate(
    state([map(1, 2 * DAY)], { lastShownAt: NOW - REVIEW_INTERVAL_MS + 1 }),
    NOW,
  );
  assert.equal(candidate, null);
});

test('через 12 часов показ снова разрешён', () => {
  const candidate = selectReviewCandidate(
    state([map(1, 2 * DAY)], { lastShownAt: NOW - REVIEW_INTERVAL_MS }),
    NOW,
  );
  assert.equal(candidate?.mapId, 1);
});

test('во время установки рекомендация разрешена только на шаге deploying', () => {
  assert.equal(reviewPrompt.isReviewInstallStep?.('deploying'), true);
  for (const step of ['checking', 'downloading', 'handing', 'analyzing', 'confirm', 'compat']) {
    assert.equal(reviewPrompt.isReviewInstallStep?.(step), false, step);
  }
});

test('при запуске показ ждёт обработки deep-link и не мешает скачиванию', () => {
  const context = {
    startupReady: false,
    page: 'home',
    installStep: 'file',
    siteStep: 'idle',
    blocked: false,
  };
  assert.equal(reviewPrompt.getReviewTrigger?.(context), null);
  assert.equal(reviewPrompt.getReviewTrigger?.({ ...context, startupReady: true }), 'home');
  assert.equal(
    reviewPrompt.getReviewTrigger?.({
      ...context,
      startupReady: true,
      page: 'install',
      installStep: 'deploying',
      siteStep: 'downloading',
    }),
    null,
  );
  assert.equal(
    reviewPrompt.getReviewTrigger?.({
      ...context,
      startupReady: true,
      page: 'install',
      installStep: 'deploying',
    }),
    'install',
  );
  for (const page of ['enter', 'oauth-callback']) {
    assert.equal(reviewPrompt.getReviewTrigger?.({ ...context, startupReady: true, page }), null);
  }
  assert.equal(
    reviewPrompt.getReviewTrigger?.({ ...context, startupReady: true, blocked: true }),
    null,
  );
});

test('повторная установка обновляет запись, а не дублирует её', () => {
  const first = state([map(1, 5 * DAY)]);
  const updated = upsertInstall(first, map(1, 0));
  assert.equal(updated.installs.length, 1);
  assert.equal(updated.installs[0].installedAt, NOW);
});

test('отметка о рекомендации ставится один раз и запоминает время', () => {
  const once = markRecommended(state([map(1, 2 * DAY)]), 1, NOW);
  const twice = markRecommended(once, 1, NOW + HOUR);
  assert.deepEqual(twice.recommended, [1]);
  assert.equal(twice.lastShownAt, NOW + HOUR);
});

test('история рекомендаций не забывает старые карты', () => {
  const recommended = Array.from({ length: 1000 }, (_, index) => index + 1);
  const updated = markRecommended(
    state([], { recommended }),
    1001,
    NOW,
  );
  assert.equal(updated.recommended.length, 1001);
  assert.equal(updated.recommended[0], 1);
  assert.equal(updated.recommended[1000], 1001);
});

test('журнал не выбрасывает старые непоказанные установки', () => {
  const installs = Array.from({ length: 1000 }, (_, index) => map(index + 1, (index + 2) * DAY));
  const updated = upsertInstall(state(installs), map(1001, DAY));
  assert.equal(updated.installs.length, 1001);
  assert.ok(updated.installs.some((item) => item.mapId === 1));
});

test('ссылка на карту строится из id', () => {
  assert.equal(buildMapUrl('https://kraimaps.ru/', 47), 'https://kraimaps.ru/maps/47');
  assert.equal(buildMapUrl('https://kraimaps.ru', 47), 'https://kraimaps.ru/maps/47');
});

test('битое хранилище сбрасывается без исключения', () => {
  assert.deepEqual(parseReviewState('{oops'), { installs: [], recommended: [], lastShownAt: 0 });
  assert.deepEqual(parseReviewState(null), { installs: [], recommended: [], lastShownAt: 0 });
});

test('мусорные записи в хранилище отбрасываются', () => {
  const raw = JSON.stringify({
    installs: [
      { mapId: 1, title: 'Хорошая', slug: 'good', installedAt: 1 },
      { mapId: 1, title: 'Дубль', slug: 'dup', installedAt: 2 },
      { mapId: -1, title: 'Минус', slug: 'bad', installedAt: 1 },
      { mapId: 2, title: '', slug: 'empty', installedAt: 1 },
    ],
    recommended: [1, 1, -3, 'x'],
    lastShownAt: 'bad',
  });
  const parsed = parseReviewState(raw);
  assert.equal(parsed.installs.length, 1);
  assert.equal(parsed.installs[0].mapId, 1);
  assert.deepEqual(parsed.recommended, [1]);
  assert.equal(parsed.lastShownAt, 0);
});

test('старые записи со slug читаются и slug в них игнорируется', () => {
  const raw = JSON.stringify({
    installs: [{ mapId: 47, title: 'Тройка', slug: 'trojka-buben', installedAt: 1 }],
  });
  const parsed = parseReviewState(raw);
  assert.equal(parsed.installs.length, 1);
  assert.equal(parsed.installs[0].mapId, 47);
  assert.equal(parsed.installs[0].slug, undefined);
});
