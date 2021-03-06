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

mod errors;
mod imports;
mod ivalues_lifting;
mod ivalues_lowering;
mod utils;

use std::cell::RefCell;
use wasmer_core::Func;

pub use errors::HostImportError;
pub(crate) use imports::create_host_import_func;

pub(self) use wasmer_core::types::Value as WValue;
pub(self) use wasmer_core::types::Type as WType;

pub(self) type Result<T> = std::result::Result<T, HostImportError>;
pub(self) type WasmModuleFunc<Args, Rets> = Box<RefCell<Option<Func<'static, Args, Rets>>>>;
pub(self) type AllocateFunc = WasmModuleFunc<i32, i32>;
pub(self) type SetResultPtrFunc = WasmModuleFunc<i32, ()>;
pub(self) type SetResultSizeFunc = WasmModuleFunc<i32, ()>;

pub(self) const ALLOCATE_FUNC_NAME: &str = "allocate";
pub(self) const SET_PTR_FUNC_NAME: &str = "set_result_ptr";
pub(self) const SET_SIZE_FUNC_NAME: &str = "set_result_size";
