/*
 * Copyright 2020 Fluence Labs Limited
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use crate::vm::config::Config;
use crate::vm::errors::FCEError;
use crate::vm::module::fce_result::FCEResult;
use crate::vm::module::{ModuleABI, ModuleAPI};

use sha2::digest::generic_array::GenericArray;
use sha2::digest::FixedOutput;
use wasmer_runtime::{compile, func, imports, Ctx, Func, Instance};
use wasmer_runtime_core::import::ImportObject;
use wasmer_runtime_core::memory::ptr::{Array, WasmPtr};
use wasmer_wasi::generate_import_object_for_version;

/// Describes Application Binary Interface of a module.
/// For more details see comment in abi.rs.
#[derive(Clone)]
pub(crate) struct ABI {
    // It is safe to use unwrap() while calling these functions because Option is used here
    // just to allow partially initialization. And all Option fields will contain Some if
    // invoking FCE::new has been succeed.
    pub(crate) allocate: Func<'static, i32, i32>,
    pub(crate) deallocate: Func<'static, (i32, i32), ()>,
    pub(crate) invoke: Func<'static, (i32, i32), i32>,
    pub(crate) store: Func<'static, (i32, i32)>,
    pub(crate) load: Func<'static, i32, i32>,
}

/// A building block of multi-modules scheme of FCE, represents one module that corresponds
/// to a one Wasm file.
pub(crate) struct FCEModule {
    instance: Instance,
    abi: ABI,
}

impl FCEModule {
    /// Creates a new virtual machine executor.
    pub fn new(wasm_bytes: &[u8], config: Config, imports: ImportObject) -> Result<Self, FCEError> {
        let logger_imports = imports! {
            "logger" => {
                "log_utf8_string" => func!(FCEModule::logger_log_utf8_string),
            },
        };
        let config_copy = config.clone();

        let mut import_object = generate_import_object_for_version(
            config.wasi_config.version,
            vec![],
            config.wasi_config.envs,
            config.wasi_config.preopened_files,
            config.wasi_config.mapped_dirs,
        );
        import_object.extend(logger_imports);
        import_object.extend(imports);
        import_object.allow_missing_functions = false;

        let instance = compile(&wasm_bytes)?.instantiate(&import_object)?;
        let abi = FCEModule::create_abi(&instance, &config_copy)?;

        Ok(Self { instance, abi })
    }

    #[rustfmt::skip]
    /// Extracts ABI from a module.
    fn create_abi(instance: &Instance, config: &Config) -> Result<ABI, FCEError> {
        unsafe {
            let allocate = std::mem::transmute::<Func<'_, i32, i32>, Func<'static, _, _>>(
                instance.exports.get(&config.allocate_fn_name)?
            );

            let deallocate = std::mem::transmute::<Func<'_, (i32, i32)>, Func<'static, _, _>>(
                instance.exports.get(&config.deallocate_fn_name)?
            );

            let invoke = std::mem::transmute::<Func<'_, (i32, i32), i32>, Func<'static, _, _>, >(
                instance.exports.get(&config.invoke_fn_name)?
            );

            let store = std::mem::transmute::<Func<'_, (i32, i32)>, Func<'static, _, _>>(
                instance.exports.get(&config.store_fn_name)?,
            );

            let load = std::mem::transmute::<Func<'_, i32, i32>, Func<'static, _, _>>(
                instance.exports.get(&config.load_fn_name)?,
            );

            Ok(ABI {
                allocate,
                deallocate,
                invoke,
                store,
                load,
            })
        }
    }

    /// Returns ABI of a module.
    /// (!) Be carefull and delete all instances of ABI before dropping corresponding module.
    /// There is no any internal ref counter due to the internals of Wasmer.
    pub fn get_abi(&self) -> &ABI {
        &self.abi
    }

    /// Prints utf8 string of the given size from the given offset. Called from the wasm.
    fn logger_log_utf8_string(ctx: &mut Ctx, offset: i32, size: i32) {
        let wasm_ptr = WasmPtr::<u8, Array>::new(offset as _);
        match wasm_ptr.get_utf8_string(ctx.memory(0), size as _) {
            Some(msg) => print!("{}", msg),
            None => print!("fce logger: incorrect UTF8 string's been supplied to logger"),
        }
    }

    /// Writes given value on the given address to module memory.
    fn write_to_mem(&mut self, address: usize, value: &[u8]) -> Result<(), FCEError> {
        let memory = self.instance.context().memory(0);

        for (byte_id, cell) in memory.view::<u8>()[address..(address + value.len())]
            .iter()
            .enumerate()
        {
            cell.set(value[byte_id]);
        }

        Ok(())
    }

    /// Reads invocation result from specified address of memory.
    fn read_result_from_mem(&self, address: usize) -> Result<Vec<u8>, FCEError> {
        let memory = self.instance.context().memory(0);

        let mut result_size: usize = 0;

        for (byte_id, cell) in memory.view::<u8>()[address..address + 4].iter().enumerate() {
            result_size |= (cell.get() as usize) << (8 * byte_id);
        }

        let mut result = Vec::<u8>::with_capacity(result_size);
        for cell in memory.view()[(address + 4) as usize..(address + result_size + 4)].iter() {
            result.push(cell.get());
        }

        Ok(result)
    }
}

impl ModuleAPI for FCEModule {
    fn invoke(&mut self, argument: &[u8]) -> Result<FCEResult, FCEError> {
        let argument_len = argument.len() as i32;

        // allocate memory for the given argument and write it to memory
        let argument_address = if argument_len != 0 {
            let address = self.allocate(argument_len)?;
            self.write_to_mem(address as usize, argument)?;
            address
        } else {
            0
        };

        // invoke a main module, read a result and deallocate it
        let result_address = <Self as ModuleABI>::invoke(self, argument_address, argument_len)?;
        let result = self.read_result_from_mem(result_address as _)?;
        self.deallocate(result_address, result.len() as i32)?;

        Ok(FCEResult::new(result))
    }

    fn compute_state_hash(
        &mut self,
    ) -> GenericArray<u8, <sha2::Sha256 as FixedOutput>::OutputSize> {
        use sha2::Digest;

        let mut hasher = sha2::Sha256::new();
        let memory = self.instance.context().memory(0);

        let wasm_ptr = WasmPtr::<u8, Array>::new(0 as _);
        let raw_mem = wasm_ptr
            .deref(memory, 0, (memory.size().bytes().0 - 1) as _)
            .expect("fce: internal error in compute_vm_state_hash");
        let raw_mem: &[u8] = unsafe { &*(raw_mem as *const [std::cell::Cell<u8>] as *const [u8]) };

        hasher.input(raw_mem);
        hasher.result()
    }
}

impl ModuleABI for FCEModule {
    fn allocate(&mut self, size: i32) -> Result<i32, FCEError> {
        Ok(self.abi.allocate.call(size)?)
    }

    fn deallocate(&mut self, ptr: i32, size: i32) -> Result<(), FCEError> {
        Ok(self.abi.deallocate.call(ptr, size)?)
    }

    fn invoke(&mut self, arg_address: i32, arg_size: i32) -> Result<i32, FCEError> {
        Ok(self.abi.invoke.call(arg_address, arg_size)?)
    }

    fn store(&mut self, address: i32, value: i32) -> Result<(), FCEError> {
        Ok(self.abi.store.call(address, value)?)
    }

    fn load(&mut self, address: i32) -> Result<i32, FCEError> {
        Ok(self.abi.load.call(address)?)
    }
}
