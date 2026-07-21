use std::error::Error;
use std::str::FromStr;

use serde::{ Deserializer, Serializer };

pub mod jpath;
mod native;

pub use crate::native::{ Arguments, InfallibleArguments, Function, Value };

pub trait Evaluator: Sized {
    type Implementation: Implementation<Evaluator = Self>;

    type Arguments: Arguments;
    type Deserializer<'de>: Deserializer<'de> where Self: 'de;
    type Error: Error
        + for<'de> From<<Self::Deserializer<'de> as Deserializer<'de>>::Error>
        + From<<Self::Serializer as Serializer>::Error>;
    type Serializer: Serializer<Ok = Self::Value>;
    type Value: Value<Evaluator = Self>;

    fn new(implementation: &Self::Implementation) -> Self;

    fn create_deserializer(&self) -> Self::Deserializer<'_>;

    fn create_serializer(&self) -> Self::Serializer;

    fn with_external_code<'a, 'b, 'c, K, V>(&'a mut self, key: &'b K, value: &'c V) -> &'a mut Self
    where
        'a: 'b,
        'a: 'c,
        K: AsRef<str>,
        V: AsRef<str>;
    
    fn with_external_variable<'a, 'b, 'c, K, V>(&'a mut self, key: &'b K, value: &'c V) -> &'a mut Self
    where
        'a: 'b,
        'a: 'c,
        K: AsRef<str>,
        V: AsRef<str>;
    
    fn with_native_function<'a, 'b, 'c, K, F>(&'a mut self, key: &'b K, func: &'c F) -> &'a mut Self
    where
        'b: 'a,
        'c: 'a,
        K: AsRef<str>,
        F: Function;
    
    fn with_top_level_argument<'a, 'b, 'c, K, V>(&'a mut self, key: &'b K, value: &'c V) -> &'a mut Self
    where
        K: AsRef<str>,
        V: AsRef<str>;
    
    fn with_top_level_code<'a, 'b, 'c, K, V>(&'a mut self, key: &'b K, value: &'c V) -> &'a mut Self
    where
        K: AsRef<str>,
        V: AsRef<str>;

    fn evaluate(self) -> Result<Self::Value, <Self as Evaluator>::Error>;
}

pub trait Flag: Sized {
    type Implementation: Implementation<Flag = Self>;
    
    type Key: FromStr;
    type Value: FromStr;
    type Error: Error
        + From<<Self::Key as FromStr>::Err>
        + From<<Self::Value as FromStr>::Err>;
    
    fn new(key: Self::Key, value: Self::Value) -> Result<Self, Self::Error>;
}

pub trait Implementation: Sized {
    type Evaluator: Evaluator<Implementation = Self>;
    type Flag: Flag<Implementation = Self>;
    type Error: Error
        + From<Self::InitializationError>
        + From<<Self::Evaluator as Evaluator>::Error>;
    type InitializationError: Error;

    fn new<'a>(flags: impl Iterator<Item = Self::Flag>) -> Result<Self, Self::InitializationError>;

    fn create_evaluator(&self) -> Self::Evaluator {
        Self::Evaluator::new(self)
    }
}
