<script setup lang="ts">
import { open } from '@tauri-apps/plugin-dialog';
import { useInstallStore } from '../../../app/stores/install';
import { api } from '../../../shared/api/commands';
import { LAUNCHER_NAMES } from '../../../shared/types/installer';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();

async function pickManualFolder() {
  const selected = await open({ multiple: false, directory: true });
  if (typeof selected === 'string') {
    store.saveManualRoot(selected);
    try {
      store.busy = true;
      const detection = await api.detectLaunchers(store.manualRoots, store.knownRoots);
      store.detection = detection;
      store.rememberRoots();
      const manual = detection.launchers.find((l) => l.kind === 'manual');
      if (manual) {
        store.selectLauncher(manual);
      } else {
        store.fail(
          {
            code: 'E_NO_LAUNCHER',
            message: 'Папка не похожа на игру',
            details: 'no game markers (saves/versions/mods/config)',
          },
          'launcher',
        );
      }
    } catch (e) {
      store.fail(e, 'launcher');
    }
  }
}
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">Куда ставим?</h2>
    <p class="mt-1 text-sm text-secondary">Выберите лаунчер. Показаны только те, что найдены на этом компьютере.</p>

    <div class="mt-4 space-y-2">
      <button
        v-for="l in store.detection?.launchers.filter((x) => (store.pack?.is_modded ? x.kind !== 'manual' : true)) ?? []"
        :key="l.kind + l.data_dir"
        type="button"
        class="glass-card w-full p-4 text-left transition-colors hover:border-white/20 cursor-pointer"
        @click="store.selectLauncher(l)"
      >
        <p class="flex items-center gap-2 text-sm font-semibold text-primary">
          {{ LAUNCHER_NAMES[l.kind] }}
          <span
            v-if="l.portable"
            class="rounded-full bg-primary-500/15 px-2 py-0.5 text-[11px] font-bold text-primary-400"
          >
            Портативный
          </span>
        </p>
        <p class="mt-0.5 truncate font-mono text-[11px] text-tertiary">{{ l.data_dir }}</p>
        <p class="mt-1 text-xs text-secondary">
          <template v-if="l.kind === 'tlauncher'">
            {{ l.targets.length === 0 ? 'Версий пока нет — установим нужную' : `Версий: ${l.targets.length}` }}
          </template>
          <template v-else>
            {{ l.targets.length === 0 ? 'Инстансов пока нет — создадим новый' : `Инстансов: ${l.targets.length}` }}
          </template>
        </p>
      </button>
    </div>

    <Alert tone="info" class="mt-4">
      <p class="text-xs">
        Нет нужного лаунчера? Запустите его и нажмите «Повторить поиск» или укажите папку вручную.
      </p>
    </Alert>

    <div class="mt-4 flex gap-2">
      <Btn variant="secondary" :disabled="store.busy" @click="store.step = 'confirm'">Назад</Btn>
      <Btn variant="secondary" :loading="store.busy" @click="store.rescanLaunchers()">
        {{ store.busy ? store.statusLine || 'Ищем…' : 'Повторить поиск' }}
      </Btn>
      <Btn variant="secondary" :disabled="store.busy" @click="pickManualFolder()">Указать папку вручную</Btn>
    </div>
  </div>
</template>
