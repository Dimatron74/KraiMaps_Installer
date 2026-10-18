<script setup lang="ts">
import { computed } from 'vue';
import { useInstallStore, VERSION_LAUNCHERS } from '../../../app/stores/install';
import { LAUNCHER_NAMES } from '../../../shared/types/installer';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();

/**
 * У лаунчеров без инстансов (TLauncher, Legacy Launcher) папка одна на все
 * версии, поэтому моды в ней общие — предупреждаем иначе. У официального
 * лаунчера наоборот: каждая наша установка со своей папкой, заменять там
 * нечего, и кнопка создания новой не нужна.
 */
const sharedFolder = computed(() =>
  store.target ? VERSION_LAUNCHERS.includes(store.target.launcher) && store.target.launcher !== 'official' : false,
);
const launcherName = computed(() =>
  store.target ? LAUNCHER_NAMES[store.target.launcher] : 'лаунчер',
);
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">
      <template v-if="sharedFolder">Внимание: общие моды</template>
      <template v-else>Внимание: чужой инстанс</template>
    </h2>
    <Alert tone="warning" class="mt-3">
      <p class="font-semibold">
        <template v-if="sharedFolder">
          Модовая карта заменит общие моды — они у {{ launcherName }} общие для всех версий.
        </template>
        <template v-else> Модовая карта заменит моды в «{{ store.target?.name }}». </template>
      </p>
      <p class="mt-1 text-xs">
        Папка <span class="font-mono">mods/</span> будет очищена и заполнена модами карты,
        остальные файлы карты (настройки, скрипты, ресурсы автора) — поставлены поверх.
        Всё удалённое и заменённое сохранится в
        <span class="font-mono">.kraimaps-backup/&lt;дата&gt;/</span> — можно откатить вручную.
        Мир и прогресс в <span class="font-mono">saves/</span> не тронем.
        <template v-if="sharedFolder">
          У {{ launcherName }} нет отдельных инстансов: моды общие для всех версий.
        </template>
      </p>
    </Alert>

    <div class="mt-5 flex flex-col gap-2">
      <Btn
        v-if="!sharedFolder"
        variant="success"
        :loading="store.busy"
        @click="store.createNewTarget()"
      >
        Создать новый инстанс (рекомендуется)
      </Btn>
      <div class="flex gap-2">
        <Btn variant="secondary" @click="store.step = 'target'">Назад</Btn>
        <Btn variant="danger" :loading="store.busy" @click="store.confirmWipe()">
          Всё равно ставить сюда
        </Btn>
      </div>
    </div>
  </div>
</template>