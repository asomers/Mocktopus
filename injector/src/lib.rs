#![feature(proc_macro)]

extern crate proc_macro;
extern crate syn;
extern crate quote;

use proc_macro::TokenStream;
use quote::{Tokens, ToTokens};
use std::mem;
use std::str::FromStr;
use syn::{BindingMode, Block, Constness, ExprKind, FnArg, Generics, Ident, ImplItem, ImplItemKind, Item, ItemKind,
        MethodSig, Mutability, Pat, Path, Stmt, Ty};

#[proc_macro_attribute]
pub fn inject_mocks(_: TokenStream, token_stream: TokenStream) -> TokenStream {
    let in_string = token_stream.to_string();
    let mut parsed = match syn::parse_item(&in_string) {
        Ok(parsed) => parsed,
        Err(_) => return token_stream,
    };
    inject_item(&mut parsed);
    let mut tokens = Tokens::new();
    parsed.to_tokens(&mut tokens);
    let out_string = tokens.as_str();
    let out_token_stream = TokenStream::from_str(out_string).unwrap();
    out_token_stream
}

fn inject_item(item: &mut Item) {
    match item.node {
        ItemKind::Mod(ref mut items_opt) =>
            inject_mod(items_opt.as_mut()),
        ItemKind::Fn(ref mut decl, _, ref constness, _, ref generics, ref mut block) =>
            inject_static_fn(&item.ident, &mut decl.inputs, constness, generics, block),
        ItemKind::Impl(_, _, ref generics, ref path, ref ty, ref mut items) =>
            inject_impl(generics, path.as_ref(), ty, items),
        //        ItemKind::Trait(ref mut unsafety, ref mut generics, ref mut ty_param_bound, ref mut items) => unimplemented!(),
        _ => (),
    }
}

fn inject_mod(items_opt: Option<&mut Vec<Item>>) {
    if let Some(items) = items_opt {
        for item in items {
            inject_item(item)
        }
    }
}

fn inject_impl(_generics: &Generics, path: Option<&Path>, _ty: &Box<Ty>, items: &mut Vec<ImplItem>) {
//    println!("PATH\n{:#?}\nTY\n{:#?}\nITEMS\n{:#?}", path, ty, items);
    if path.is_some() {
        return; // no trait support yet
    }
    for item in items {
        if let ImplItemKind::Method(
            MethodSig {
                unsafety: _,
                constness: ref constness_ref,
                abi: _,
                decl: ref mut decl_ref,
                generics: ref generics_ref },
            ref mut block) = item.node {
            match decl_ref.inputs.get(0) { // no non-static methods support yet
                Some(&FnArg::SelfRef(..)) | Some(&FnArg::SelfValue(..)) => continue,
                _ => (),
            };
            let mut full_fn_name = format!("Self::{}", item.ident.as_ref());
            append_generics(&mut full_fn_name, generics_ref);
            inject_fn(&full_fn_name, &mut decl_ref.inputs, constness_ref, block);
        }
    }

//    pub struct MethodSig {
//        pub unsafety: Unsafety,
//        pub constness: Constness,
//        pub abi: Option<Abi>,
//        pub decl: FnDecl,
//        pub generics: Generics,
//    }


//    pub struct ImplItem {
//        pub ident: Ident,
//        pub vis: Visibility,
//        pub defaultness: Defaultness,
//        pub attrs: Vec<Attribute>,
//        pub node: ImplItemKind,
//    }


    // impl [<path> for] ty {
    //      <items>
    // }


}

fn inject_static_fn(ident: &Ident, inputs: &mut Vec<FnArg>, constness: &Constness, generics: &Generics, block: &mut Box<Block>) {
    let mut full_fn_name = ident.to_string();
    append_generics(&mut full_fn_name, generics);
    inject_fn(&full_fn_name, inputs, constness, block);
}

fn inject_fn(full_fn_name: &str, inputs: &mut Vec<FnArg>, constness: &Constness, block: &mut Block) {
    if *constness == Constness::Const {
        return
    }
    unignore_fn_args(inputs);
    let mut header_builder = HeaderBuilder::default();
    header_builder.set_input_args(inputs);
    let header_stmts = header_builder.build(full_fn_name.to_string());
    let mut body_stmts = mem::replace(&mut block.stmts, header_stmts);
    block.stmts.append(&mut body_stmts);
}

fn unignore_fn_args(inputs: &mut Vec<FnArg>) {
    for i in 0..inputs.len() {
        let unignored = match inputs[i] {
            FnArg::Captured(Pat::Wild, ref ty) =>
                FnArg::Captured(
                    Pat::Ident(
                        BindingMode::ByValue(
                            Mutability::Immutable),
                        Ident::from(format!("__mock_unignored_argument_{}__", i)),
                        None),
                    ty.clone()),
            _ => continue,
        };
        inputs[i] = unignored;
    }
}

fn append_generics(fn_name: &mut String, generics: &Generics) {
    if generics.ty_params.is_empty() {
        return
    }
    fn_name.push_str("::<");
    for ty_param in &generics.ty_params {
        fn_name.push_str(&ty_param.ident.as_ref());
        fn_name.push(',');
    }
    fn_name.push('>');
}

#[derive(Default)]
struct HeaderBuilder<'a> {
    input_args: Option<&'a Vec<FnArg>>,
}

impl<'a> HeaderBuilder<'a> {
    pub fn build(self, full_fn_name: String) -> Vec<Stmt> {
        let input_args_str = self.create_input_args_str();
        let header_str = format!(
            r#"{{
            let ({}) = {{
                use mocktopus::*;
                match Mockable::call_mock(&{}, (({}))) {{
                    MockResult::Continue(input) => input,
                    MockResult::Return(result) => return result,
                }}
            }};
        }}"#, input_args_str, full_fn_name, input_args_str);
        let header_expr = syn::parse_expr(&header_str).expect("Mocktopus internal error: generated header unparsable");
        match header_expr.node {
            ExprKind::Block(_, block) => block.stmts,
            _ => panic!("Mocktopus internal error: generated header not a block"),
        }
    }

    pub fn set_input_args(&mut self, inputs: &'a Vec<FnArg>) {
        self.input_args = Some(inputs);
    }

    fn create_input_args_str(&self) -> String {
        let mut result = String::new();
        for input_arg in self.input_args.expect("Mocktopus internal error: inputs not set") {
            match *input_arg {
                FnArg::SelfRef(_, _) | FnArg::SelfValue(_) => result.push_str("self"),
                FnArg::Captured(Pat::Ident(_, ref ident, None), _) => result.push_str(ident.as_ref()),
                _ => panic!("Mocktopus internal error: invalid function input '{:?}'", input_arg),
            };
            result.push_str(", ");
        };
        result
    }
}
