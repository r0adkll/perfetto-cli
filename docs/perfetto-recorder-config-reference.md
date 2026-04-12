# Perfetto Recorder UI — Configuration Reference

How each option in the [Perfetto recorder UI](https://ui.perfetto.dev/#!/record)
maps to the generated `TraceConfig` textproto
(visible at `#!/record/cmdline`).

Source of truth: `ui/src/plugins/dev.perfetto.RecordTraceV2/` in
[google/perfetto](https://github.com/google/perfetto).

---

## Table of Contents

- [1. Buffers & Duration](#1-buffers--duration)
- [2. CPU](#2-cpu)
- [3. GPU](#3-gpu)
- [4. Power](#4-power)
- [5. Memory](#5-memory)
- [6. Android Apps & Svcs](#6-android-apps--svcs)
- [7. Chrome Browser](#7-chrome-browser)
- [8. Perfetto SDK](#8-perfetto-sdk)
- [9. Stack Sampling](#9-stack-sampling)
- [10. Network](#10-network)
- [11. Advanced Settings](#11-advanced-settings)
- [Appendix A — Atrace Categories](#appendix-a--atrace-categories)
- [Appendix B — Presets](#appendix-b--presets)

---

## 1. Buffers & Duration

Top-level recording parameters. Every trace starts here.

| UI Control | Type | Values | Default | TraceConfig field(s) |
|---|---|---|---|---|
| Recording mode | Radio | `STOP_WHEN_FULL`, `RING_BUFFER`, `LONG_TRACE` | `STOP_WHEN_FULL` | `buffers[].fill_policy` (`DISCARD` for stop-when-full, `RING_BUFFER` otherwise); `LONG_TRACE` also sets `write_into_file: true` |
| In-memory buffer size | Slider (MB) | 4, 8, 16, 32, 64, 128, 256, 512 | 64 | `buffers[].size_kb` |
| Max duration | Slider | 10s, 15s, 30s, 60s, 5m, 30m, 1h, 6h, 12h | 10s (6h for LONG_TRACE) | `duration_ms` |
| Max file size | Slider (MB) | 5, 25, 50, 100, 500, 1000, 5000, 10000 | 500 | `max_file_size_bytes` (only when LONG_TRACE) |
| Flush period | Slider (ms) | 100, 250, 500, 1000, 2500, 5000 | 2500 | `file_write_period_ms` (only when LONG_TRACE) |
| Deflate compression | Toggle | on / off | off | `compression_type: COMPRESSION_TYPE_DEFLATE` |

### Buffer allocation rules

- A **default** buffer is always created at the configured size.
- Some probes create **dedicated buffers** (e.g. `proc_assoc` at 1/16th of
  default, clamped 256–8192 KB; network probe with its own buffer).
- Each data source targets a buffer by name; the builder resolves names to
  indices at serialization time.
- Fill policy per buffer: `DISCARD` when mode is `STOP_WHEN_FULL` (or buffer
  explicitly marked `DISCARD`), `RING_BUFFER` otherwise.

---

## 2. CPU

**Page:** "CPU" — *CPU usage, scheduling, wakeups*
**Platforms:** Android, Linux, ChromeOS

### 2.1 Coarse CPU usage counter

Poll-based CPU time and fork stats.

| Setting | Type | Values | Default |
|---|---|---|---|
| Poll interval | Slider (ms) | 250, 500, 1000, 2500, 5000, 30000, 60000 | 1000 |

**Generated config:**

```textproto
data_sources {
  config {
    name: "linux.sys_stats"
    sys_stats_config {
      stat_period_ms: <pollMs>
      stat_counters: STAT_CPU_TIMES
      stat_counters: STAT_FORK_COUNT
    }
  }
}
```

**Dependency:** Enables *Process/thread association* (§11.2) automatically.

### 2.2 Scheduling details

Per-CPU context switches, wakeups, and blocked reasons.

No user-configurable settings.

**Generated config** (ftrace events):

```
sched/sched_switch
power/suspend_resume
sched/sched_blocked_reason
sched/sched_wakeup
sched/sched_wakeup_new
sched/sched_waking
sched/sched_process_exit
sched/sched_process_free
task/task_newtask
task/task_rename
```

**Dependencies:** Enables *Advanced ftrace config* (§11.1) and
*Process/thread association* (§11.2).

### 2.3 CPU frequency and idle states

Frequency changes and C-state transitions, plus poll-based freq snapshots.

| Setting | Type | Values | Default |
|---|---|---|---|
| Poll interval | Slider (ms) | 250, 500, 1000, 2500, 5000, 30000, 60000 | 1000 |

**Generated config:**

```textproto
data_sources {
  config {
    name: "linux.sys_stats"
    sys_stats_config {
      cpufreq_period_ms: <pollMs>
    }
  }
}
```

Plus ftrace events:

```
power/cpu_frequency
power/cpu_idle
power/suspend_resume
```

### 2.4 Syscalls

Raw syscall enter/exit tracing. High overhead.

No user-configurable settings.

**Generated config** (ftrace events):

```
raw_syscalls/sys_enter
raw_syscalls/sys_exit
```

---

## 3. GPU

**Page:** "GPU" — *GPU frequency, memory*
**Platforms:** Varies per probe (noted below)

### 3.1 GPU frequency

**Platforms:** Android, Linux, ChromeOS

**Generated config** (ftrace event):

```
power/gpu_frequency
```

### 3.2 GPU memory

**Platforms:** Android only

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.gpu.memory"
  }
}
```

Plus ftrace event:

```
gpu_mem/gpu_mem_total
```

### 3.3 GPU work period

**Platforms:** Android only

**Generated config** (ftrace event):

```
power/gpu_work_period
```

### 3.4 GPU render stages

**Platforms:** Android only

**Generated config:**

```textproto
data_sources {
  config {
    name: "gpu.renderstages"
    target_buffer: <default>
  }
}
```

### 3.5 Mali GPU counters

**Platforms:** Android only

**Generated config:**

```textproto
data_sources {
  config {
    name: "gpu.counters"
    target_buffer: <default>
    gpu_counter_config {
      counter_period_ns: 100000
      counter_ids: <large array of Mali counter IDs>
    }
  }
}
```

### 3.6 Mali fence events

**Platforms:** Android only

Creates a **separate** `linux.ftrace` data source instance:

```textproto
data_sources {
  config {
    name: "linux.ftrace"
    target_buffer: <default>
    ftrace_config {
      ftrace_events: "mali/mali_KCPU_FENCE_SIGNAL"
      ftrace_events: "mali/mali_KCPU_FENCE_WAIT_END"
      ftrace_events: "mali/mali_KCPU_FENCE_WAIT_START"
    }
  }
}
```

---

## 4. Power

**Page:** "Power" — *Battery and other energy counters*

### 4.1 Battery drain & power rails

**Platforms:** Android only

| Setting | Type | Values | Default |
|---|---|---|---|
| Poll interval | Slider (ms) | 250, 500, 1000, 2500, 5000, 30000, 60000 | 1000 |

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.power"
    android_power_config {
      battery_poll_ms: <pollMs>
      collect_power_rails: true
      battery_counters: BATTERY_COUNTER_CAPACITY_PERCENT
      battery_counters: BATTERY_COUNTER_CHARGE
      battery_counters: BATTERY_COUNTER_CURRENT
    }
  }
}
```

### 4.2 Board voltages & frequencies

**Platforms:** Android, Linux, ChromeOS

No user-configurable settings.

**Generated config** (ftrace events):

```
regulator/regulator_set_voltage
regulator/regulator_set_voltage_complete
power/clock_enable
power/clock_disable
power/clock_set_rate
power/suspend_resume
```

---

## 5. Memory

**Page:** "Memory" — *Physical mem, VM, LMK*

### 5.1 Native heap profiling

**Platforms:** Android, Linux

| Setting | Type | Values | Default |
|---|---|---|---|
| Target processes | Textarea | Process names or PIDs | — |
| Sampling interval | Slider (bytes) | 1 – 1048576 | 4096 |
| Continuous dump interval | Slider (ms) | 0 – 3600000 (0 = end only) | 0 |
| Continuous dump phase | Slider (ms) | 0 – 3600000 | 0 |
| Shared memory buffer | Slider (KB) | 1024 – 131072 | 8192 |
| Block client | Toggle | on / off | on |
| All heaps | Toggle | on / off | off |

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.heapprofd"
    heapprofd_config {
      sampling_interval_bytes: <samplingBytes>
      shmem_size_bytes: <shmemKB * 1024>
      block_client: <blockClient>
      all_heaps: <allHeaps>
      process_cmdline: "<proc1>"
      process_cmdline: "<proc2>"
      pid: <numericPid>
      continuous_dump_config {
        dump_interval_ms: <dumpInterval>
        dump_phase_ms: <dumpPhase>
      }
    }
  }
}
```

### 5.2 Java heap dumps

**Platforms:** Android only

| Setting | Type | Values | Default |
|---|---|---|---|
| Target processes | Textarea | Process names or PIDs | — |
| Continuous dump interval | Slider (ms) | 0 – 3600000 | 0 |
| Continuous dump phase | Slider (ms) | 0 – 3600000 | 0 |

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.java_hprof"
    java_hprof_config {
      process_cmdline: "<proc>"
      pid: <numericPid>
      continuous_dump_config {
        dump_interval_ms: <dumpInterval>
        dump_phase_ms: <dumpPhase>
      }
    }
  }
}
```

### 5.3 Kernel meminfo

**Platforms:** Android, Linux, ChromeOS

| Setting | Type | Values | Default |
|---|---|---|---|
| Poll interval | Slider (ms) | 250 – 60000 | 1000 |
| Counters | Multiselect | All `MeminfoCounters` enum values | — |

**Generated config:**

```textproto
data_sources {
  config {
    name: "linux.sys_stats"
    sys_stats_config {
      meminfo_period_ms: <pollMs>
      meminfo_counters: MEMINFO_MEM_TOTAL
      meminfo_counters: MEMINFO_MEM_FREE
      meminfo_counters: ...
    }
  }
}
```

### 5.4 Virtual memory stats

**Platforms:** Android, Linux, ChromeOS

| Setting | Type | Values | Default |
|---|---|---|---|
| Poll interval | Slider (ms) | 250 – 60000 | 1000 |
| Counters | Multiselect | All `VmstatCounters` enum values | — |

**Generated config:**

```textproto
data_sources {
  config {
    name: "linux.sys_stats"
    sys_stats_config {
      vmstat_period_ms: <pollMs>
      vmstat_counters: VMSTAT_NR_FREE_PAGES
      vmstat_counters: ...
    }
  }
}
```

### 5.5 High-frequency memory events

**Platforms:** Android only

No user-configurable settings.

**Generated config** (ftrace events):

```
mm_event/mm_event_record
kmem/rss_stat
ion/ion_stat
dmabuf_heap/dma_heap_stat
kmem/ion_heap_grow
kmem/ion_heap_shrink
```

**Dependency:** Enables *Process/thread association* (§11.2).

### 5.6 Low memory killer

**Platforms:** Android, Linux, ChromeOS

No user-configurable settings.

**Generated config** (ftrace events + atrace app):

```
lowmemorykiller/lowmemory_kill
oom/oom_score_adj_update
```

Plus atrace app: `lmkd`

**Dependency:** Enables *Process/thread association* (§11.2).

### 5.7 Per-process stats polling

**Platforms:** Android, Linux, ChromeOS

| Setting | Type | Values | Default |
|---|---|---|---|
| Poll interval | Slider (ms) | 250 – 60000 | 1000 |
| Record process age | Toggle | on / off | off |
| Record process runtime | Toggle | on / off | off |

**Generated config:**

```textproto
data_sources {
  config {
    name: "linux.process_stats"
    target_buffer: <proc_assoc buffer index>
    process_stats_config {
      proc_stats_poll_ms: <pollMs>
      record_process_age: <procAge>
      record_process_runtime: <procRuntime>
    }
  }
}
```

**Dependency:** Enables *Process/thread association* (§11.2).

---

## 6. Android Apps & Svcs

**Page:** "Android apps & svcs"
**Platforms:** Android (except where noted)

### 6.1 Atrace userspace annotations

The primary mechanism for Android app/framework tracing.

| Setting | Type | Values | Default |
|---|---|---|---|
| Categories | Multiselect | 37 atrace categories (see [Appendix A](#appendix-a--atrace-categories)) | See appendix (those marked `isDefault`) |
| Apps / processes | Textarea | Package names or process names | — |
| Trace all apps | Toggle | on / off | off |

**Generated config:**

When categories or apps are selected, these are added to the shared
`linux.ftrace` data source:

```textproto
data_sources {
  config {
    name: "linux.ftrace"
    ftrace_config {
      atrace_categories: "gfx"
      atrace_categories: "sched"
      atrace_categories: ...
      atrace_apps: "<app1>"    # or "*" if "trace all apps" is on
      atrace_apps: "<app2>"
      ftrace_events: "ftrace/print"   # always added when any category is selected
    }
  }
}
```

### 6.2 Event log (logcat)

| Setting | Type | Values | Default |
|---|---|---|---|
| Log buffers | Multiselect | Crash, Main, Binary events, Kernel, Radio, Security, Stats, System | — |

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.log"
    android_log_config {
      log_ids: LID_CRASH
      log_ids: LID_DEFAULT
      log_ids: LID_EVENTS
      log_ids: ...
    }
  }
}
```

### 6.3 Frame timeline

No user-configurable settings.

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.surfaceflinger.frametimeline"
  }
}
```

### 6.4 Game intervention list

No user-configurable settings.

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.game_interventions"
  }
}
```

### 6.5 Network tracing

| Setting | Type | Values | Default |
|---|---|---|---|
| Poll interval | Slider (ms) | 250 – 60000 | 1000 |

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.network_packets"
    network_packet_trace_config {
      poll_ms: <pollMs>
    }
  }
}
data_sources {
  config {
    name: "android.packages_list"
  }
}
```

### 6.6 Statsd atoms

| Setting | Type | Values | Default |
|---|---|---|---|
| Push atoms | Multiselect | All push AtomIds (2 < id < 9999) | — |
| Raw push atom IDs | Textarea | Comma-separated IDs | — |
| Pull atoms | Multiselect | All pull AtomIds (10000 < id < 99999) | — |
| Raw pull atom IDs | Textarea | Comma-separated IDs | — |
| Pull frequency | Slider (ms) | Standard poll intervals | 5000 |
| Pull packages | Textarea | Package names | — |

**Generated config:**

```textproto
data_sources {
  config {
    name: "android.statsd"
    statsd_tracing_config {
      push_atom_id: <id>
      push_atom_id: ...
      raw_push_atom_id: <id>
      pull_config {
        pull_atom_id: <id>
        pull_frequency_ms: <pullInterval>
        packages: "<pkg>"
      }
    }
  }
}
```

---

## 7. Chrome Browser

**Page:** "Chrome browser"
**Platforms:** Chrome

A single compound probe: **Chrome browser tracing**.

### Settings

| Setting | Type | Description |
|---|---|---|
| Enabled tags | Multiselect | Chrome trace tags to include |
| Disabled tags | Multiselect | Chrome trace tags to exclude |
| Presets | Multiselect | Groups of categories (see below) |
| Privacy filtering | Toggle | "Remove untyped and sensitive data like URLs" |
| Manual categories | Multiselect | Full list of 200+ Chrome trace categories |

### Category presets

| Preset | Categories included |
|---|---|
| Task Scheduling | `toplevel`, `toplevel.flow`, `scheduler`, `sequence_manager`, `disabled-by-default-toplevel.flow` |
| IPC Flows | `toplevel`, `toplevel.flow`, `disabled-by-default-ipc.flow`, `mojom` |
| Javascript execution | `toplevel`, `v8` |
| Web content rendering | `toplevel`, `blink`, `cc`, `gpu` |
| UI rendering & compositing | `toplevel`, `cc`, `gpu`, `viz`, `ui`, `views` |
| Input events | `toplevel`, `benchmark`, `evdev`, `input`, `disabled-by-default-toplevel.flow` |
| Navigation & loading | `loading`, `net`, `netlog`, `navigation`, `browser` |
| Audio | `audio`, `webaudio`, `webrtc`, + others |
| Video | `media`, `gpu`, `webrtc`, + others |

### Generated config

Chrome tracing produces multiple data sources:

**1. Track event (always):**

```textproto
data_sources {
  config {
    name: "track_event"
    track_event_config {
      disabled_categories: "*"
      enabled_categories: "<cat1>"
      enabled_categories: "<cat2>"
      enabled_categories: ...
      enable_thread_time_sampling: true
      timestamp_unit_multiplier: 1000
      # if privacy filtering is on:
      filter_dynamic_event_names: true
      filter_debug_annotations: true
    }
  }
}
```

**2. Metadata buffer (always):**

A dedicated 256 KB buffer with `DISCARD` fill policy:

```textproto
buffers {
  size_kb: 256
  fill_policy: DISCARD
}
data_sources {
  config {
    name: "org.chromium.trace_metadata2"
    target_buffer: <metadata buffer index>
  }
}
```

**3. Conditional data sources** (added when relevant categories are enabled):

| Condition | Data source |
|---|---|
| `memory-infra` category | `org.chromium.memory_instrumentation` + `org.chromium.native_heap_profiler` |
| `cpu_profiler` category | `org.chromium.sampler_profiler` |
| Always (when Chrome tracing on) | `org.chromium.system_metrics`, `org.chromium.histogram_sample` |

**4. Chrome JSON config** (embedded in the data source):

```json
{
  "record_mode": "record-until-full",
  "included_categories": ["<cat1>", "<cat2>"],
  "excluded_categories": ["*"],
  "memory_dump_config": {}
}
```

---

## 8. Perfetto SDK

**Page:** "Perfetto SDK" — *Track events*
**Platforms:** Android, Linux

| Setting | Type | Values | Default |
|---|---|---|---|
| Categories | Multiselect | `mq` (Message Queue), `gfx` (Graphics), `servicemanager` (Service Manager) | — |
| Additional categories | Textarea | Freeform category names | — |

**Generated config:**

```textproto
data_sources {
  config {
    name: "track_event"
    track_event_config {
      disabled_categories: "*"
      enabled_categories: "<cat1>"
      enabled_categories: "<cat2>"
    }
  }
}
```

---

## 9. Stack Sampling

**Page:** "Stack sampling" — *Callstack sampling*
**Platforms:** Android, Linux

| Setting | Type | Values | Default |
|---|---|---|---|
| Sampling frequency | Slider (Hz) | 1, 10, 50, 100, 250, 500, 1000 | 100 |
| Target processes | Textarea | Process names | — |

**Generated config:**

```textproto
data_sources {
  config {
    name: "linux.perf"
    perf_event_config {
      timebase {
        frequency: <samplingFreq>
        timestamp_clock: PERF_CLOCK_MONOTONIC
      }
      callstack_sampling {
        scope {
          target_cmdline: "<proc1>"
          target_cmdline: "<proc2>"
        }
      }
    }
  }
}
```

---

## 10. Network

**Page:** "Network" — *Wi-Fi / network ftrace events*
**Platforms:** Android, Linux, ChromeOS

| Setting | Type | Values | Default |
|---|---|---|---|
| 802.11 layer events | Toggle | on / off | off |
| Packets TX/RX | Toggle | on / off | off |
| Dedicated buffer size | Slider (MB) | 0, 4, 8, 16, 32, 64, 128, 256, 512 (0 = use default) | 0 |
| Additional driver events | Textarea | Ftrace event names | — |

**Generated config:**

When **802.11 layer events** is on, adds ftrace events:

```
cfg80211/*
mac80211/*
```

When **Packets TX/RX** is on, adds ftrace events:

```
net/netif_receive_skb
net/net_dev_xmit
```

If a **dedicated buffer** is configured (> 0), a separate `linux.ftrace` data
source is created targeting its own buffer instead of the shared one:

```textproto
buffers {
  size_kb: <bufSizeMb * 1024>
}
data_sources {
  config {
    name: "linux.ftrace"
    target_buffer: <network buffer index>
    ftrace_config {
      ftrace_events: "cfg80211/*"
      ftrace_events: "net/netif_receive_skb"
      ftrace_events: ...
    }
  }
}
```

Additional driver events from the textarea are appended to `ftrace_events`.

---

## 11. Advanced Settings

### 11.1 Advanced ftrace config

**Platforms:** Android, Linux, ChromeOS

Controls the shared `linux.ftrace` data source that all ftrace-based probes
contribute events to.

| Setting | Type | Values | Default |
|---|---|---|---|
| Resolve kernel symbols | Toggle | on / off | on |
| Enable generic events (slow) | Toggle | on / off | off |
| Ftrace buffer size | Slider (KB) | 0, 512, 1024, 2048, 4096, 16384, 32768 (0 = kernel default) | 0 |
| Ftrace drain rate | Slider (ms) | 0, 100, 250, 500, 1000, 2500, 5000 (0 = default) | 0 |
| Ftrace event groups | Multiselect | binder/\*, block/\*, clk/\*, devfreq/\*, ext4/\*, f2fs/\*, i2c/\*, irq/\*, kmem/\*, memory_bus/\*, mmc/\*, oom/\*, power/\*, regulator/\*, sched/\*, sync/\*, task/\*, vmscan/\*, fastrpc/\* | — |

**Generated config** (added to the shared `linux.ftrace` data source):

```textproto
data_sources {
  config {
    name: "linux.ftrace"
    ftrace_config {
      symbolize_ksyms: <ksyms>
      disable_generic_events: <inverse of genericEvents>
      buffer_size_kb: <bufSize>        # omitted if 0
      drain_period_ms: <drainRate>     # omitted if 0
      ftrace_events: "binder/*"
      ftrace_events: "block/*"
      ftrace_events: ...
    }
  }
}
```

### 11.2 Process/thread association

**Platforms:** Android, Linux, ChromeOS

Automatically enabled as a dependency by many probes (CPU usage, CPU
scheduling, high-freq memory events, LMK, per-process stats).

| Setting | Type | Values | Default |
|---|---|---|---|
| Scan all processes at startup | Toggle | on / off | on |

**Generated config:**

Creates a **dedicated `proc_assoc` buffer** (1/16th of the default buffer
size, clamped to 256–8192 KB).

Ftrace events (added to shared data source):

```
sched/sched_process_exit
sched/sched_process_free
task/task_newtask
task/task_rename
```

Data source:

```textproto
data_sources {
  config {
    name: "linux.process_stats"
    target_buffer: <proc_assoc buffer index>
    process_stats_config {
      scan_all_processes_on_start: <initialScan>
    }
  }
}
```

---

## Appendix A — Atrace Categories

Full list of atrace categories available in the Android apps & svcs section.
Categories marked **default** are pre-selected.

| ID | Label | Default |
|---|---|---|
| `adb` | ADB | |
| `aidl` | AIDL calls | **yes** |
| `am` | Activity Manager | **yes** |
| `audio` | Audio | |
| `binder_driver` | Binder Kernel driver | **yes** |
| `binder_lock` | Binder global lock trace | |
| `bionic` | Bionic C Library | |
| `camera` | Camera | **yes** |
| `dalvik` | Dalvik VM | **yes** |
| `database` | Database | |
| `disk` | Disk I/O | **yes** |
| `freq` | CPU Frequency | **yes** |
| `gfx` | Graphics | **yes** |
| `hal` | Hardware Modules | **yes** |
| `idle` | CPU Idle | **yes** |
| `input` | Input | **yes** |
| `memory` | Memory | **yes** |
| `memreclaim` | Kernel Memory Reclaim | **yes** |
| `network` | Network | **yes** |
| `nnapi` | NNAPI | |
| `pm` | Package Manager | |
| `power` | Power Management | **yes** |
| `res` | Resource Loading | **yes** |
| `rro` | Runtime Resource Overlay | |
| `rs` | RenderScript | |
| `sched` | CPU Scheduling | **yes** |
| `sm` | Sync Manager | |
| `ss` | System Server | **yes** |
| `sync` | Synchronization | **yes** |
| `thermal` | Thermal event | **yes** |
| `vibrator` | Vibrator | |
| `video` | Video | |
| `view` | View System | **yes** |
| `webview` | WebView | **yes** |
| `wm` | Window Manager | **yes** |
| `workq` | Kernel Workqueues | **yes** |

---

## Appendix B — Presets

The recorder UI offers one-click presets that configure multiple probes at
once.

### Android presets

| Preset | Probes enabled | Buffer | Duration |
|---|---|---|---|
| **Default** | CPU usage (1s), CPU scheduling, CPU freq (1s), Atrace (defaults + all apps), Logcat (Default/System/Crash/Events), Frame timeline | 64 MB | 10s |
| **Battery** | Atrace (battery cats + all apps), Power rails (1s), CPU usage (1s) | 64 MB | 30s |
| **Thermal** | CPU scheduling, Atrace (thermal cats + all apps), Power rails (1s), Board voltages, CPU usage (1s), CPU freq (1s) | 64 MB | 30s |
| **Graphics** | CPU usage, CPU scheduling, CPU freq, GPU freq, GPU memory, GPU work period, Per-process stats, Frame timeline, Atrace (graphics cats + all apps), Advanced ftrace (power group) | 64 MB | 30s |

### Linux presets

| Preset | Probes enabled | Buffer | Duration |
|---|---|---|---|
| **Default** | CPU usage, CPU scheduling, CPU freq, Process stats, Sys stats | 64 MB | 10s |
| **Scheduling** | CPU scheduling, CPU freq (100ms), Process stats (100ms) | 64 MB | 10s |

### Chrome presets

| Preset | Category presets | Buffer | Duration |
|---|---|---|---|
| **Default** | Task Scheduling, Javascript, Web content rendering, UI rendering, Input events, Navigation/loading | 256 MB | 30s |
| **V8** | Task Scheduling, Javascript, Navigation/loading + explicit `disabled-by-default-v8.gc`, `disabled-by-default-v8.compile`, `disabled-by-default-v8.cpu_profiler` | 256 MB | 30s |
