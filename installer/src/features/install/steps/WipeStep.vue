<script setup lang="ts">
import { useInstallStore } from '../../../app/stores/install';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">
      <template v-if="store.target?.launcher === 'tlauncher'">Внимание: общие моды TLauncher</template>
      <template v-else>Внимание: чужой инстанс</template>
    </h2>
    <Alert tone="warning" class="mt-3">
      <p class="font-semibold">
        <template v-if="store.target?.launcher === 'tlauncher'">
          Модовая карта заменит общие моды TLauncher.
        </template>
        <template v-else> Модовая карта заменит моды в «{{ store.target?.name }}». </template>
      </p>
      <p class="mt-1 text-xs">
        Папка <span class="font-mono">mods/</span> будет очищена и заполнена модами карты,
        остальные файлы карты (настройки, скрипты, ресурсы автора) — поставлены поверх.
        Всё удалённое и заменённое сохранится в
        <span class="font-mono">.kraimaps-backup/&lt;дата&gt;/</span> — можно откатить вручную.
        Мир и прогресс в <span class="font-mono">saves/</span> не тронем.
        <template v-if="store.target?.launcher === 'tlauncher'">
          У TLauncher нет отдельных инстансов: моды общие для всех версий.
        </template>
      </p>
    </Alert>

    <div class="mt-5 flex flex-col gap-2">
      <Btn
        v-if="store.target?.launcher !== 'tlauncher'"
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
