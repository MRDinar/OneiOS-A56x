// SPDX-License-Identifier: MIT
//! cpu/: the host's topology, the vCPU plan, thread placement and the virtual cpufreq device.

pub mod topology; // cpu/topology.c

pub use topology::{
    set_affinity, set_uclamp_min, CoreKind, CpuPlan, Cpufreq, HostCpu, TidOf, Topology, Vcpu, CPUFREQ_WINDOW, MAX_CPUS,
    MAX_FREQS, MAX_VCPUS,
};

#[cfg(test)]
mod test_cpu; // tests/test_bridges.c's test_cpu
