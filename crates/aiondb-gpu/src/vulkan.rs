//! Vulkan GPU batch distance computation via wgpu + WGSL compute shaders.

use std::sync::Arc;

use aiondb_core::{DbError, DbResult};
use tracing::info;
use wgpu::util::DeviceExt;

use crate::{BatchDistanceComputer, DistanceMetric};

/// GPU-accelerated batch distance computer using Vulkan via wgpu.
pub struct GpuBatchDistance {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    l2_pipeline: wgpu::ComputePipeline,
    cosine_pipeline: wgpu::ComputePipeline,
    ip_pipeline: wgpu::ComputePipeline,
    manhattan_pipeline: wgpu::ComputePipeline,
}

impl std::fmt::Debug for GpuBatchDistance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuBatchDistance")
            .field("backend", &"vulkan-gpu")
            .finish_non_exhaustive()
    }
}

/// Uniform params struct matching the WGSL Params layout.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuParams {
    dims: u32,
    count: u32,
}

impl GpuBatchDistance {
    /// Initialize the GPU device and compile compute pipelines.
    ///
    /// Returns an error if no Vulkan-capable GPU is available.
    pub fn new() -> DbResult<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| DbError::internal("no Vulkan-capable GPU adapter found"))?;

        let adapter_info = adapter.get_info();
        info!(
            name = adapter_info.name,
            driver = adapter_info.driver,
            backend = ?adapter_info.backend,
            "GPU adapter selected"
        );

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aiondb-gpu"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| DbError::internal(format!("GPU device request failed: {e}")))?;

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let l2_pipeline = create_pipeline(&device, "l2", include_str!("shaders/l2.wgsl"));
        let cosine_pipeline =
            create_pipeline(&device, "cosine", include_str!("shaders/cosine.wgsl"));
        let ip_pipeline = create_pipeline(
            &device,
            "inner_product",
            include_str!("shaders/inner_product.wgsl"),
        );
        let manhattan_pipeline =
            create_pipeline(&device, "manhattan", include_str!("shaders/manhattan.wgsl"));

        Ok(Self {
            device,
            queue,
            l2_pipeline,
            cosine_pipeline,
            ip_pipeline,
            manhattan_pipeline,
        })
    }

    fn pipeline_for(&self, metric: DistanceMetric) -> &wgpu::ComputePipeline {
        match metric {
            DistanceMetric::L2 => &self.l2_pipeline,
            DistanceMetric::Cosine => &self.cosine_pipeline,
            DistanceMetric::InnerProduct => &self.ip_pipeline,
            DistanceMetric::Manhattan => &self.manhattan_pipeline,
        }
    }

    fn dispatch(
        &self,
        query: &[f32],
        targets_flat: &[f32],
        dims: usize,
        count: usize,
        pipeline: &wgpu::ComputePipeline,
    ) -> DbResult<Vec<f32>> {
        let params = GpuParams {
            dims: dims as u32,
            count: count as u32,
        };

        // Create GPU buffers.
        let query_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("query"),
                contents: bytemuck::cast_slice(query),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let targets_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("targets"),
                contents: bytemuck::cast_slice(targets_flat),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let output_size = (count * std::mem::size_of::<f32>()) as u64;
        let output_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("distances"),
            size: output_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let staging_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging"),
            size: output_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Bind group.
        let bind_group_layout = pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("distance_bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: query_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: targets_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        });

        // Encode and submit.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("distance_encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("distance_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let workgroups = (count as u32).div_ceil(64);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buf, 0, &staging_buf, 0, output_size);
        self.queue.submit(std::iter::once(encoder.finish()));

        // Read back results.
        let slice = staging_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|e| DbError::internal(format!("GPU readback channel error: {e}")))?
            .map_err(|e| DbError::internal(format!("GPU buffer map failed: {e}")))?;

        let data = slice.get_mapped_range();
        let result: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        staging_buf.unmap();

        Ok(result)
    }
}

/// Below this count, GPU dispatch overhead exceeds compute savings.
/// Small batches fall back to CPU scalar.
const GPU_DISPATCH_THRESHOLD: usize = 64;

impl BatchDistanceComputer for GpuBatchDistance {
    fn compute_distances(
        &self,
        query: &[f32],
        targets_flat: &[f32],
        dims: usize,
        metric: DistanceMetric,
    ) -> DbResult<Vec<f32>> {
        if dims == 0 || targets_flat.is_empty() {
            return Ok(Vec::new());
        }
        if query.len() != dims {
            return Err(aiondb_core::DbError::internal(format!(
                "vulkan compute_distances: query length {} does not match dims {dims}",
                query.len()
            )));
        }
        if targets_flat.len() % dims != 0 {
            return Err(aiondb_core::DbError::internal(format!(
                "vulkan compute_distances: targets buffer length {} is not a multiple of dims {dims}",
                targets_flat.len()
            )));
        }
        let count = targets_flat.len() / dims;
        // For small batches, GPU dispatch overhead is not worth it.
        // Fall back to CPU scalar computation.
        if count < GPU_DISPATCH_THRESHOLD {
            return crate::CpuBatchDistance.compute_distances(query, targets_flat, dims, metric);
        }
        let pipeline = self.pipeline_for(metric);
        self.dispatch(query, targets_flat, dims, count, pipeline)
    }

    fn backend_name(&self) -> &'static str {
        "vulkan-gpu"
    }
}

fn create_pipeline(
    device: &wgpu::Device,
    label: &str,
    shader_source: &str,
) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader_source.into()),
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}
