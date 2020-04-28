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
use crate::vm::errors::FrankError;
use crate::vm::module::abi::ModuleABI;
use crate::vm::module::frank_result::FrankResult;
use crate::vm::module::ModuleAPI;

use sha2::digest::generic_array::GenericArray;
use sha2::digest::FixedOutput;
use wasmer_runtime::{compile, func, imports, Ctx, Func, Instance};
use wasmer_runtime_core::import::ImportObject;
use wasmer_runtime_core::memory::ptr::{Array, WasmPtr};
use wasmer_wasi::generate_import_object_for_version;

pub struct FrankModule {
    instance: &'static Instance,

    // It is safe to use unwrap() while calling these functions because Option is used here
    // just to allow partially initialization. And all Option fields will contain Some if
    // invoking Frank::new has been succeed.
    /// Allocates a region of memory inside a module. Used for passing argument inside the module.
    allocate: Option<Func<'static, i32, i32>>,

    /// Deallocates previously allocated memory region.
    deallocate: Option<Func<'static, (i32, i32), ()>>,

    /// Calls the main entry point of a module called invoke.
    invoke: Option<Func<'static, (i32, i32), i32>>,

    /// Stores one given byte on provided address.
    store: Option<Func<'static, (i32, i32)>>,

    /// Loads one bytes from provided address.
    load: Option<Func<'static, i32, i32>>,
}

impl FrankModule {
    /// Creates a new virtual machine executor.
    pub fn new(
        wasm_bytes: &[u8],
        config: Config,
        imports: ImportObject,
    ) -> Result<Self, FrankError> {
        let logger_imports = imports! {
            "logger" => {
                "log_utf8_string" => func!(FrankModule::logger_log_utf8_string),
            },
        };

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
        let instance: &'static mut Instance = Box::leak(Box::new(instance));

        Ok(Self {
            instance,
            allocate: Some(instance.exports.get(&config.allocate_fn_name)?),
            deallocate: Some(instance.exports.get(&config.deallocate_fn_name)?),
            invoke: Some(instance.exports.get(&config.invoke_fn_name)?),
            store: Some(instance.exports.get(&config.store_fn_name)?),
            load: Some(instance.exports.get(&config.load_fn_name)?),
        })
    }

    /// Prints utf8 string of the given size from the given offset. Called from the wasm.
    fn logger_log_utf8_string(ctx: &mut Ctx, offset: i32, size: i32) {
        let wasm_ptr = WasmPtr::<u8, Array>::new(offset as _);
        match wasm_ptr.get_utf8_string(ctx.memory(0), size as _) {
            Some(msg) => print!("{}", msg),
            None => print!("frank logger: incorrect UTF8 string's been supplied to logger"),
        }
    }

    /// Writes given value on the given address to module memory.
    fn write_to_mem(&mut self, address: usize, value: &[u8]) -> Result<(), FrankError> {
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
    fn read_result_from_mem(&self, address: usize) -> Result<Vec<u8>, FrankError> {
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

impl ModuleABI for FrankModule {
    fn allocate(&mut self, size: i32) -> i32 {
        self.allocate
            .as_ref()
            .unwrap()
            .call(size)
            .expect("allocate failed")
    }

    fn deallocate(&mut self, ptr: i32, size: i32) {
        self.deallocate
            .as_ref()
            .unwrap()
            .call(ptr, size)
            .expect("allocate failed");
    }

    fn invoke(&mut self, ptr: i32, size: i32) -> i32 {
        self.invoke
            .as_ref()
            .unwrap()
            .call(ptr, size)
            .expect("invoke failed")
    }

    fn load(&self, ptr: i32) -> i32 {
        self.load.as_ref().unwrap().call(ptr).expect("load failed")
    }

    fn store(&mut self, ptr: i32, value: i32) {
        self.store
            .as_ref()
            .unwrap()
            .call(ptr, value)
            .expect("store failed");
    }
}

impl ModuleAPI for FrankModule {
    fn invoke(&mut self, argument: &[u8]) -> Result<FrankResult, FrankError> {
        // allocate memory for the given argument and write it to memory
        let argument_len = argument.len() as i32;
        let argument_address = if argument_len != 0 {
            let address = self.allocate.as_ref().unwrap().call(argument_len)?;
            self.write_to_mem(address as usize, argument)?;
            address
        } else {
            0
        };

        // invoke a main module, read a result and deallocate it
        let result_address = self
            .invoke
            .as_ref()
            .unwrap()
            .call(argument_address, argument_len)?;
        let result = self.read_result_from_mem(result_address as _)?;

        self.deallocate
            .as_ref()
            .unwrap()
            .call(result_address, result.len() as i32)?;

        Ok(FrankResult::new(result))
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
            .expect("frank: internal error in compute_vm_state_hash");
        let raw_mem: &[u8] = unsafe { &*(raw_mem as *const [std::cell::Cell<u8>] as *const [u8]) };

        hasher.input(raw_mem);
        hasher.result()
    }
}

impl Drop for FrankModule {
    // The manually drop is needed because at first we need to delete functions
    // and only then instance.
    fn drop(&mut self) {
        #[allow(clippy::drop_copy)]
        drop(self.allocate.as_ref());

        #[allow(clippy::drop_copy)]
        drop(self.deallocate.as_ref());

        #[allow(clippy::drop_copy)]
        drop(self.invoke.as_ref());

        #[allow(clippy::drop_copy)]
        drop(self.store.as_ref());

        #[allow(clippy::drop_copy)]
        drop(self.load.as_ref());

        // delete instance
        unsafe {
            // let _ = Box::from_raw(self.instance as *mut FrankModule);
        }
    }
}