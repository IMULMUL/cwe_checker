use std::collections::{HashMap, HashSet};

use super::{Expression, ExpressionType, RegisterProperties, Variable};
use crate::intermediate_representation::Arg as IrArg;
use crate::intermediate_representation::Blk as IrBlk;
use crate::intermediate_representation::ByteSize;
use crate::intermediate_representation::CallingConvention as IrCallingConvention;
use crate::intermediate_representation::Def as IrDef;
use crate::intermediate_representation::Expression as IrExpression;
use crate::intermediate_representation::ExternSymbol as IrExternSymbol;
use crate::intermediate_representation::Jmp as IrJmp;
use crate::intermediate_representation::Program as IrProgram;
use crate::intermediate_representation::Project as IrProject;
use crate::intermediate_representation::Sub as IrSub;
use crate::prelude::*;
use crate::utils::log::LogMessage;

// TODO: Handle the case where an indirect tail call is represented by CALLIND plus RETURN

// TODO: Since we do not support BAP anymore, this module should be refactored
// to remove BAP-specific artifacts like the jump label type.

/// A call instruction.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Call {
    /// The target label. May be `None` for `CALLOTHER` instructions.
    pub target: Option<Label>,
    /// The return label if the call is expected to return.
    #[serde(rename = "return")]
    pub return_: Option<Label>,
    /// A description of the instruction for `CALLOTHER` instructions.
    pub call_string: Option<String>,
}

/// A jump instruction.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Jmp {
    /// The mnemonic of the jump.
    pub mnemonic: JmpType,
    /// The target label for intraprocedural jumps.
    pub goto: Option<Label>,
    /// The call struct for interprocedural jumps.
    pub call: Option<Call>,
    /// If the jump is a conditional jump,
    /// the varnode that has to evaluate to `true` for the jump to be taken.
    pub condition: Option<Variable>,
    /// A list of potential jump targets for indirect jumps.
    pub target_hints: Option<Vec<String>>,
}

/// A jump type mnemonic.
#[allow(missing_docs)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub enum JmpType {
    BRANCH,
    CBRANCH,
    BRANCHIND,
    CALL,
    CALLIND,
    CALLOTHER,
    RETURN,
}

impl From<Jmp> for IrJmp {
    /// Convert a P-Code jump to the internally used IR.
    fn from(jmp: Jmp) -> IrJmp {
        use JmpType::*;
        let unwrap_label_direct = |label| {
            if let Label::Direct(tid) = label {
                tid
            } else {
                panic!()
            }
        };
        let unwrap_label_indirect = |label| {
            if let Label::Indirect(expr) = label {
                expr
            } else {
                panic!()
            }
        };
        match jmp.mnemonic {
            BRANCH => IrJmp::Branch(unwrap_label_direct(jmp.goto.unwrap())),
            CBRANCH => IrJmp::CBranch {
                target: unwrap_label_direct(jmp.goto.unwrap()),
                condition: jmp.condition.unwrap().into(),
            },
            BRANCHIND => {
                let target = unwrap_label_indirect(jmp.goto.unwrap());
                IrJmp::BranchInd(target.into())
            }
            CALL => {
                let call = jmp.call.unwrap();
                IrJmp::Call {
                    target: unwrap_label_direct(call.target.unwrap()),
                    return_: call.return_.map(unwrap_label_direct),
                }
            }
            CALLIND => {
                let call = jmp.call.unwrap();
                IrJmp::CallInd {
                    target: unwrap_label_indirect(call.target.unwrap()).into(),
                    return_: call.return_.map(unwrap_label_direct),
                }
            }
            CALLOTHER => {
                let call = jmp.call.unwrap();
                IrJmp::CallOther {
                    description: call.call_string.unwrap(),
                    return_: call.return_.map(unwrap_label_direct),
                }
            }
            RETURN => IrJmp::Return(unwrap_label_indirect(jmp.goto.unwrap()).into()),
        }
    }
}

/// A jump label for distinguishing between direct and indirect jumps.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub enum Label {
    /// The term identifier of the target of a direct jump.
    Direct(Tid),
    /// The varnode holding the target address of an indirect jump.
    Indirect(Variable),
}

/// An assignment instruction, assigning the result of an expression to a varnode.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Def {
    /// The target varnode whose value gets overwritten.
    pub lhs: Option<Variable>,
    /// The expression that determines the value to be written.
    pub rhs: Expression,
}

impl From<Def> for IrDef {
    /// Convert a P-Code instruction to the internally used IR.
    fn from(def: Def) -> IrDef {
        use super::ExpressionType::*;
        match def.rhs.mnemonic {
            LOAD => IrDef::Load {
                var: def.lhs.unwrap().into(),
                address: def.rhs.input1.unwrap().into(),
            },
            STORE => IrDef::Store {
                address: def.rhs.input1.unwrap().into(),
                value: def.rhs.input2.unwrap().into(),
            },
            SUBPIECE => IrDef::Assign {
                var: def.lhs.clone().unwrap().into(),
                value: IrExpression::Subpiece {
                    low_byte: def.rhs.input1.unwrap().parse_to_bytesize(),
                    size: def.lhs.unwrap().size,
                    arg: Box::new(def.rhs.input0.unwrap().into()),
                },
            },
            INT_ZEXT | INT_SEXT | INT2FLOAT | FLOAT2FLOAT | TRUNC | POPCOUNT => IrDef::Assign {
                var: def.lhs.clone().unwrap().into(),
                value: IrExpression::Cast {
                    op: def.rhs.mnemonic.into(),
                    size: def.lhs.unwrap().size,
                    arg: Box::new(def.rhs.input0.unwrap().into()),
                },
            },
            _ => {
                let target_var = def.lhs.unwrap();
                if target_var.address.is_some() {
                    IrDef::Store {
                        address: IrExpression::Const(target_var.parse_to_bitvector()),
                        value: def.rhs.into(),
                    }
                } else {
                    IrDef::Assign {
                        var: target_var.into(),
                        value: def.rhs.into(),
                    }
                }
            }
        }
    }
}

/// A basic block.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Blk {
    /// The `Def` instructions of the block in chronological order.
    pub defs: Vec<Term<Def>>,
    /// The jump instructions at the end of the basic block.
    pub jmps: Vec<Term<Jmp>>,
}

impl From<Blk> for IrBlk {
    /// Convert a P-Code block to the internally used IR.
    fn from(blk: Blk) -> IrBlk {
        let defs: Vec<Term<IrDef>> = blk
            .defs
            .into_iter()
            .map(|def_term| Term {
                tid: def_term.tid,
                term: def_term.term.into(),
            })
            .collect();
        let indirect_jmp_targets = blk
            .jmps
            .iter()
            .find_map(|jmp_term| jmp_term.term.target_hints.clone())
            .unwrap_or_default();
        let jmps: Vec<Term<IrJmp>> = blk
            .jmps
            .into_iter()
            .map(|jmp_term| Term {
                tid: jmp_term.tid,
                term: jmp_term.term.into(),
            })
            .collect();
        IrBlk {
            defs,
            jmps,
            indirect_jmp_targets,
        }
    }
}

impl Blk {
    /// Add `LOAD` instructions for implicit memory accesses
    /// to convert them to explicit memory accesses.
    fn add_load_defs_for_implicit_ram_access(&mut self, generic_pointer_size: ByteSize) {
        let mut refactored_defs = Vec::new();
        for def in self.defs.iter() {
            let mut cleaned_def = def.clone();
            if let Some(input) = &def.term.rhs.input0 {
                if input.address.is_some() {
                    let load_def = input.to_load_def("$load_temp0", generic_pointer_size);
                    cleaned_def.term.rhs.input0 = load_def.lhs.clone();
                    refactored_defs.push(Term {
                        tid: def.tid.clone().with_id_suffix("_load0"),
                        term: load_def,
                    });
                }
            }
            if let Some(input) = &def.term.rhs.input1 {
                if input.address.is_some() {
                    let load_def = input.to_load_def("$load_temp1", generic_pointer_size);
                    cleaned_def.term.rhs.input1 = load_def.lhs.clone();
                    refactored_defs.push(Term {
                        tid: def.tid.clone().with_id_suffix("_load1"),
                        term: load_def,
                    });
                }
            }
            if let Some(input) = &def.term.rhs.input2 {
                if input.address.is_some() {
                    let load_def = input.to_load_def("$load_temp2", generic_pointer_size);
                    cleaned_def.term.rhs.input2 = load_def.lhs.clone();
                    refactored_defs.push(Term {
                        tid: def.tid.clone().with_id_suffix("_load2"),
                        term: load_def,
                    });
                }
            }
            refactored_defs.push(cleaned_def);
        }

        for (index, jmp) in self.jmps.iter_mut().enumerate() {
            match jmp.term.mnemonic {
                JmpType::BRANCHIND | JmpType::CALLIND => {
                    let input = match jmp.term.mnemonic {
                        JmpType::BRANCHIND => match jmp.term.goto.as_mut().unwrap() {
                            Label::Indirect(expr) => expr,
                            Label::Direct(_) => panic!(),
                        },
                        JmpType::CALLIND => {
                            match jmp.term.call.as_mut().unwrap().target.as_mut().unwrap() {
                                Label::Indirect(expr) => expr,
                                Label::Direct(_) => panic!(),
                            }
                        }
                        _ => panic!(),
                    };
                    if input.address.is_some() {
                        let temp_register_name = format!("$load_temp{}", index);
                        let load_def = input.to_load_def(&temp_register_name, generic_pointer_size);
                        *input = load_def.lhs.clone().unwrap();
                        refactored_defs.push(Term {
                            tid: jmp.tid.clone().with_id_suffix("_load"),
                            term: load_def,
                        });
                    }
                }
                _ => (),
            }
        }
        self.defs = refactored_defs;
    }
}

/// An argument (parameter or return value) of an extern symbol.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Arg {
    /// The register containing the argument if it is passed in a register.
    pub var: Option<Variable>,
    /// The expression computing the location of the argument if it is passed on the stack.
    pub location: Option<Expression>,
    /// The intent (input or output) of the argument.
    pub intent: ArgIntent,
}

/// The intent (input or output) of a function argument.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
#[allow(clippy::upper_case_acronyms)]
pub enum ArgIntent {
    /// The argument is an input parameter.
    INPUT,
    /// The argument is a return value.
    OUTPUT,
}

/// A subfunction.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Sub {
    /// The name of the function.
    pub name: String,
    /// The basic blocks of the function.
    ///
    /// Note that the first block of the array may *not* be the function entry point!
    pub blocks: Vec<Term<Blk>>,
}

impl From<Term<Sub>> for Term<IrSub> {
    /// Convert a `Sub` term in the P-Code representation to a `Sub` term in the intermediate representation.
    /// The conversion also repairs the order of the basic blocks in the `blocks` array of the `Sub`
    /// in the sense that the first block of the array is required to also be the function entry point
    /// after the conversion.
    fn from(mut sub: Term<Sub>) -> Term<IrSub> {
        // Since the intermediate representation expects that the first block of a function is its entry point,
        // we have to make sure that this actually holds.
        if !sub.term.blocks.is_empty() && sub.tid.address != sub.term.blocks[0].tid.address {
            let mut start_block_index = None;
            for (i, block) in sub.term.blocks.iter().enumerate() {
                if block.tid.address == sub.tid.address {
                    start_block_index = Some(i);
                    break;
                }
            }
            if let Some(start_block_index) = start_block_index {
                sub.term.blocks.swap(0, start_block_index);
            } else {
                panic!("Non-empty function without correct starting block encountered. Name: {}, TID: {}", sub.term.name, sub.tid);
            }
        }

        let blocks = sub
            .term
            .blocks
            .into_iter()
            .map(|block_term| Term {
                tid: block_term.tid,
                term: block_term.term.into(),
            })
            .collect();
        Term {
            tid: sub.tid,
            term: IrSub {
                name: sub.term.name,
                blocks,
            },
        }
    }
}

/// An extern symbol, i.e. a function not contained in the binary but loaded from a shared library.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct ExternSymbol {
    /// The term identifier of the extern symbol.
    pub tid: Tid,
    /// The addresses to call the extern symbol.
    /// May be more than one, since we also identify thunk functions calling the extern symbol with the symbol itself.
    pub addresses: Vec<String>,
    /// The name of the extern symbol.
    pub name: String,
    /// The calling convention used (as reported by Ghidra, i.e. this may not be correct).
    pub calling_convention: Option<String>,
    /// The input and output arguments of the function.
    pub arguments: Vec<Arg>,
    /// If the function is assumed to never return to the caller, this flag is set to `true`.
    pub no_return: bool,
}

impl From<ExternSymbol> for IrExternSymbol {
    /// Convert an extern symbol parsed from Ghidra to the internally used IR.
    fn from(symbol: ExternSymbol) -> IrExternSymbol {
        let mut parameters = Vec::new();
        let mut return_values = Vec::new();
        for arg in symbol.arguments {
            let ir_arg = if let Some(var) = arg.var {
                IrArg::Register(var.into())
            } else if let Some(expr) = arg.location {
                if expr.mnemonic == ExpressionType::LOAD {
                    IrArg::Stack {
                        offset: i64::from_str_radix(
                            expr.input0
                                .clone()
                                .unwrap()
                                .address
                                .unwrap()
                                .trim_start_matches("0x"),
                            16,
                        )
                        .unwrap(),
                        size: expr.input0.unwrap().size,
                    }
                } else {
                    panic!()
                }
            } else {
                panic!()
            };
            match arg.intent {
                ArgIntent::INPUT => parameters.push(ir_arg),
                ArgIntent::OUTPUT => return_values.push(ir_arg),
            }
        }
        IrExternSymbol {
            tid: symbol.tid,
            addresses: symbol.addresses,
            name: symbol.name,
            calling_convention: symbol.calling_convention,
            parameters,
            return_values,
            no_return: symbol.no_return,
        }
    }
}

/// The program struct containing all information about the binary
/// except for CPU-architecture-related information.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Program {
    /// The subfunctions contained in the binary.
    pub subs: Vec<Term<Sub>>,
    /// The extern symbols referenced by the binary.
    pub extern_symbols: Vec<ExternSymbol>,
    /// The term identifiers of entry points into the binary.
    pub entry_points: Vec<Tid>,
    /// The base address of the memory image of the binary in RAM as reported by Ghidra.
    ///
    /// Note that Ghidra may add an offset to the image base address as reported by the binary itself.
    pub image_base: String,
}

impl Program {
    /// Convert a program parsed from Ghidra to the internally used IR.
    ///
    /// The `binary_base_address` denotes the base address of the memory image of the binary
    /// according to the program headers of the binary.
    /// It is needed to detect whether Ghidra added a constant offset to all addresses of the memory address.
    /// E.g. if the `binary_base_address` is 0 for shared object files,
    /// Ghidra adds an offset so that the memory image does not actually start at address 0.
    pub fn into_ir_program(self, binary_base_address: u64) -> IrProgram {
        let subs = self.subs.into_iter().map(|sub| sub.into()).collect();
        let extern_symbols = self
            .extern_symbols
            .into_iter()
            .map(|symbol| symbol.into())
            .collect();
        let address_base_offset =
            u64::from_str_radix(&self.image_base, 16).unwrap() - binary_base_address;
        IrProgram {
            subs,
            extern_symbols,
            entry_points: self.entry_points,
            address_base_offset,
        }
    }
}

/// A struct describing a calling convention.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct CallingConvention {
    /// The name of the calling convention.
    #[serde(rename = "calling_convention")]
    pub name: String,
    /// Possible parameter registers.
    parameter_register: Vec<String>,
    /// Possible return registers.
    return_register: Vec<String>,
    /// Callee-saved registers.
    unaffected_register: Vec<String>,
    /// Registers that may be overwritten by the call, i.e. caller-saved registers.
    killed_by_call_register: Vec<String>,
}

impl From<CallingConvention> for IrCallingConvention {
    fn from(cconv: CallingConvention) -> IrCallingConvention {
        IrCallingConvention {
            name: cconv.name,
            parameter_register: cconv.parameter_register,
            return_register: cconv.return_register,
            callee_saved_register: cconv.unaffected_register,
        }
    }
}

/// The project struct describing all known information about the binary.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
pub struct Project {
    /// The program struct containing all binary-specific information.
    pub program: Term<Program>,
    /// The CPU-architecture that the binary uses.
    pub cpu_architecture: String,
    /// The stack pointer register of the CPU-architecture.
    pub stack_pointer_register: Variable,
    /// Information about all CPU-architecture-specific registers.
    pub register_properties: Vec<RegisterProperties>,
    /// Information about known calling conventions for the given CPU architecture.
    pub register_calling_convention: Vec<CallingConvention>,
}

impl Project {
    /// Convert a project parsed from Ghidra to the internally used IR.
    ///
    /// The `binary_base_address` denotes the base address of the memory image of the binary
    /// according to the program headers of the binary.
    pub fn into_ir_project(self, binary_base_address: u64) -> IrProject {
        let mut program: Term<IrProgram> = Term {
            tid: self.program.tid,
            term: self.program.term.into_ir_program(binary_base_address),
        };
        let register_map: HashMap<&String, &RegisterProperties> = self
            .register_properties
            .iter()
            .map(|p| (&p.register, p))
            .collect();
        let mut zero_extend_tids: HashSet<Tid> = HashSet::new();
        // iterates over definitions and checks whether sub registers are used
        // if so, they are swapped with subpieces of base registers
        for sub in program.term.subs.iter_mut() {
            for blk in sub.term.blocks.iter_mut() {
                let mut def_iter = blk.term.defs.iter_mut().peekable();
                while let Some(def) = def_iter.next() {
                    let peeked_def = def_iter.peek();
                    match &mut def.term {
                        IrDef::Assign { var, value } => {
                            if let Some(zero_tid) = value
                                .cast_sub_registers_to_base_register_subpieces(
                                    Some(var),
                                    &register_map,
                                    peeked_def,
                                )
                            {
                                zero_extend_tids.insert(zero_tid);
                            }
                        }
                        IrDef::Load { var, address } => {
                            if let Some(zero_tid) = address
                                .cast_sub_registers_to_base_register_subpieces(
                                    Some(var),
                                    &register_map,
                                    peeked_def,
                                )
                            {
                                zero_extend_tids.insert(zero_tid);
                            }
                        }
                        IrDef::Store { address, value } => {
                            address.cast_sub_registers_to_base_register_subpieces(
                                None,
                                &register_map,
                                peeked_def,
                            );
                            value.cast_sub_registers_to_base_register_subpieces(
                                None,
                                &register_map,
                                peeked_def,
                            );
                        }
                    }
                }
                for jmp in blk.term.jmps.iter_mut() {
                    match &mut jmp.term {
                        IrJmp::BranchInd(dest) => {
                            dest.cast_sub_registers_to_base_register_subpieces(
                                None,
                                &register_map,
                                None,
                            );
                        }
                        IrJmp::CBranch { condition, .. } => {
                            condition.cast_sub_registers_to_base_register_subpieces(
                                None,
                                &register_map,
                                None,
                            );
                        }
                        IrJmp::CallInd { target, .. } => {
                            target.cast_sub_registers_to_base_register_subpieces(
                                None,
                                &register_map,
                                None,
                            );
                        }
                        IrJmp::Return(dest) => {
                            dest.cast_sub_registers_to_base_register_subpieces(
                                None,
                                &register_map,
                                None,
                            );
                        }
                        _ => (),
                    }
                }
                // Remove all tagged zero extension instruction that came after a sub register instruction
                // since it has been wrapped around the former instruction.
                blk.term.defs.retain(|def| {
                    if zero_extend_tids.contains(&def.tid) {
                        return false;
                    }
                    true
                });
            }
        }
        IrProject {
            program,
            cpu_architecture: self.cpu_architecture,
            stack_pointer_register: self.stack_pointer_register.into(),
            calling_conventions: self
                .register_calling_convention
                .into_iter()
                .map(|cconv| cconv.into())
                .collect(),
        }
    }
}

impl Project {
    /// This function runs normalization passes to bring the project into a form
    /// that can be translated into the internally used intermediate representation.
    ///
    /// Currently implemented normalization passes:
    ///
    /// ### Insert explicit `LOAD` instructions for implicit memory loads in P-Code.
    ///
    /// Ghidra generates implicit loads for memory accesses, whose address is a constant.
    /// The pass converts them to explicit `LOAD` instructions.
    ///
    /// ### Remove basic blocks of functions without correct starting block
    ///
    /// Sometimes Ghidra generates a (correct) function start inside another function.
    /// But if the function start is not also the start of a basic block,
    /// we cannot handle it correctly (yet) as this would need splitting of basic blocks.
    /// So instead we generate a log message and handle the function as a function without code,
    /// i.e. a dead end in the control flow graph.
    #[must_use]
    pub fn normalize(&mut self) -> Vec<LogMessage> {
        let mut log_messages = Vec::new();

        // Insert explicit `LOAD` instructions for implicit memory loads in P-Code.
        let generic_pointer_size = self.stack_pointer_register.size;
        for sub in self.program.term.subs.iter_mut() {
            for block in sub.term.blocks.iter_mut() {
                block
                    .term
                    .add_load_defs_for_implicit_ram_access(generic_pointer_size);
            }
        }

        // remove all blocks from functions that have no correct starting block and generate a log-message.
        for sub in self.program.term.subs.iter_mut() {
            if !sub.term.blocks.is_empty()
                && sub.tid.address != sub.term.blocks[0].tid.address
                && sub
                    .term
                    .blocks
                    .iter()
                    .find(|block| block.tid.address == sub.tid.address)
                    .is_none()
            {
                log_messages.push(LogMessage::new_error(format!(
                    "Starting block of function {} ({}) not found.",
                    sub.term.name, sub.tid
                )));
                sub.term.blocks = Vec::new();
            }
        }

        log_messages
    }
}

#[cfg(test)]
mod tests;
