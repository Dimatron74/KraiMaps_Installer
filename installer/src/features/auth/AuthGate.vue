<script setup lang="ts">
/**
 * Экран входа при запуске через kraimaps://install/{id} без сессии.
 * Тексты простые, без технических терминов и без кринжа.
 */
import { onMounted, ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { useAuthStore } from '../../app/stores/auth';
import Btn from '../../shared/ui/Btn.vue';
import ErrorDetails from '../../shared/ui/ErrorDetails.vue';

const route = useRoute();
const router = useRouter();
const auth = useAuthStore();

const mapId = ref<number | null>(null);

/** Уже авторизован (например, сессия вернулась ретраем) — экрану входа тут нечего делать. */
function leaveIfAuthed() {
  if (auth.isAuthed) router.push('/');
}

onMounted(() => {
  const q = route.query.mapId;
  const raw = Array.isArray(q) ? q[0] : q;
  const n = raw !== undefined ? Number(raw) : NaN;
  mapId.value = Number.isInteger(n) && (n as number) > 0 ? (n as number) : auth.pendingMapId;
  if (auth.status === 'unknown') void auth.refresh().then(leaveIfAuthed);
  else leaveIfAuthed();
});

watch(
  () => auth.isAuthed,
  (v) => {
    if (v) router.push('/');
  },
);

async function login() {
  // Ошибка уже записана в auth.error внутри startLogin — здесь гасим
  // реджект, чтобы не было unhandled rejection в консоли.
  try {
    await auth.startLogin(mapId.value);
  } catch {
    /* noop */
  }
}

function withoutAuth() {
  auth.continueWithoutAuth(mapId.value);
  router.push('/');
}
</script>

<template>
  <div class="flex flex-col items-center py-6 text-center">
    <div class="w-full max-w-md rounded-2xl border border-white/10 bg-white/[0.03] p-6 backdrop-blur-md">
      <h2 class="text-xl font-bold text-primary">Установка карты</h2>
      <p class="mt-1 text-sm text-secondary">Через установщик KraiMaps</p>

      <ul class="mt-5 space-y-3 text-left">
        <li class="flex gap-3">
          <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-primary-500/15 text-primary-300" aria-hidden="true">
            <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path stroke-linecap="round" stroke-linejoin="round" d="M13 10V3L4 14h7v7l9-11h-7z" /></svg>
          </span>
          <span>
            <span class="block text-sm font-semibold text-primary">Ставится сама</span>
            <span class="block text-xs text-secondary">Ничего скачивать и распаковывать вручную не нужно.</span>
          </span>
        </li>
        <li class="flex gap-3">
          <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-primary-500/15 text-primary-300" aria-hidden="true">
            <svg class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path stroke-linecap="round" stroke-linejoin="round" d="M12 8v4l3 3m6-3a9 9 0 1 1-18 0 9 9 0 0 1 18 0z" /></svg>
          </span>
          <span>
            <span class="block text-sm font-semibold text-primary">История в профиле</span>
            <span class="block text-xs text-secondary">Видно, какие карты уже устанавливал.</span>
          </span>
        </li>
        <li class="flex gap-3">
          <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-primary-500/15 text-primary-300" aria-hidden="true">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="h-4 w-4"><path d="m15.477 12.89 1.515 8.526a.5.5 0 0 1-.81.47l-3.58-2.687a1 1 0 0 0-1.197 0l-3.586 2.686a.5.5 0 0 1-.81-.469l1.514-8.526"/><circle cx="12" cy="8" r="6"/></svg>
          </span>
          <span>
            <span class="block text-sm font-semibold text-primary">Учитывается для наград</span>
            <span class="block text-xs text-secondary">Установки сохраняются и идут в зачёт.</span>
          </span>
        </li>
        <li class="flex gap-3">
          <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-primary-500/15 text-primary-300" aria-hidden="true">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" class="h-4 w-4"><path d="M11.013 18.582 6.396 21.01a.53.53 0 0 1-.77-.56l.881-5.139a2.12 2.12 0 0 0-.611-1.879L2.16 9.795a.53.53 0 0 1 .294-.906l5.165-.755a2.12 2.12 0 0 0 1.597-1.16l2.309-4.679a.53.53 0 0 1 .95 0l2.31 4.679a2.12 2.12 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904L20 11.5"/><path d="M15 18h6"/><path d="M18 15v6"/></svg>
          </span>
          <span>
            <span class="block text-sm font-semibold text-primary">Глубокая интеграция с профилем</span>
            <span class="block text-xs text-secondary">В будущем возможны дополнительные функции.</span>
          </span>
        </li>
      </ul>

      <Btn
        class="mt-6 w-full"
        size="lg"
        :loading="auth.status === 'authorizing'"
        @click="login"
      >
        Войти через сайт
      </Btn>
      <button
        type="button"
        class="mt-3 cursor-pointer text-xs text-tertiary underline-offset-2 hover:text-primary hover:underline"
        @click="withoutAuth"
      >
        Установить без авторизации
      </button>

      <ErrorDetails
        v-if="auth.error"
        class="mt-4 text-left"
        :code="auth.errorCode ?? 'E_UNKNOWN'"
        :message="auth.error"
        :details="auth.errorDetails ?? ''"
        hint="Попробуй войти ещё раз. Если ошибка повторяется — раскрой «Подробнее» и покажи код поддержке."
      />
    </div>
  </div>
</template>
