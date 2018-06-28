// Copyright (c) 2018  Brendan Molloy <brendan@bbqsrc.net>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

pub extern crate gherkin_rust as gherkin;
pub extern crate regex;

use gherkin::{Step, StepType, Feature};
use regex::Regex;
use std::collections::HashMap;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::prelude::*;
use std::ops::Deref;
use std::panic;
use std::path::Path;
use std::sync::Mutex;

pub trait World: Default {}

pub struct HashableRegex(pub Regex);

impl Hash for HashableRegex {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_str().hash(state);
    }
}

impl PartialEq for HashableRegex {
    fn eq(&self, other: &HashableRegex) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for HashableRegex {}

impl Deref for HashableRegex {
    type Target = Regex;

    fn deref(&self) -> &Regex {
        &self.0
    }
}

type TestFn<T> = fn(&mut T, &Step) -> ();
type TestRegexFn<T> = fn(&mut T, &[String], &Step) -> ();


pub struct TestCase<T: Default> {
    pub test: TestFn<T>
}

impl<T: Default> TestCase<T> {
    #[allow(dead_code)]
    pub fn new(test: TestFn<T>) -> TestCase<T> {
        TestCase {
            test: test
        }
    }
}

pub struct RegexTestCase<T: Default> {
    pub test: TestRegexFn<T>
}

impl<T: Default> RegexTestCase<T> {
    #[allow(dead_code)]
    pub fn new(test: TestRegexFn<T>) -> RegexTestCase<T> {
        RegexTestCase {
            test: test
        }
    }
}

pub struct Steps<T: Default> {
    pub given: HashMap<&'static str, TestCase<T>>,
    pub when: HashMap<&'static str, TestCase<T>>,
    pub then: HashMap<&'static str, TestCase<T>>,
    pub regex: RegexSteps<T>
}

pub struct RegexSteps<T: Default> {
    pub given: HashMap<HashableRegex, RegexTestCase<T>>,
    pub when: HashMap<HashableRegex, RegexTestCase<T>>,
    pub then: HashMap<HashableRegex, RegexTestCase<T>>,
}

pub enum TestCaseType<'a, T> where T: 'a, T: Default {
    Normal(&'a TestCase<T>),
    Regex(&'a RegexTestCase<T>, Vec<String>)
}

impl<T: Default> Steps<T> {
    #[allow(dead_code)]
    pub fn new() -> Steps<T> {
        let regex_tests = RegexSteps {
            given: HashMap::new(),
            when: HashMap::new(),
            then: HashMap::new()
        };

        let tests = Steps {
            given: HashMap::new(),
            when: HashMap::new(),
            then: HashMap::new(),
            regex: regex_tests
        };

        tests
    }

    #[allow(dead_code)]
    fn test_type<'a>(&'a self, step: &Step, value: &str) -> Option<TestCaseType<'a, T>> {
        let test_bag = match step.ty {
            StepType::Given => &self.given,
            StepType::When => &self.when,
            StepType::Then => &self.then
        };

        match test_bag.get(value) {
            Some(v) => Some(TestCaseType::Normal(v)),
            None => {
                let regex_bag = match step.ty {
                    StepType::Given => &self.regex.given,
                    StepType::When => &self.regex.when,
                    StepType::Then => &self.regex.then
                };

                let result = regex_bag.iter()
                    .find(|(regex, _)| regex.is_match(&value));

                match result {
                    Some((regex, tc)) => {
                        let thing = regex.0.captures(&value).unwrap();
                        let matches: Vec<String> = thing.iter().map(|x| x.unwrap().as_str().to_string()).collect();
                        Some(TestCaseType::Regex(tc, matches))
                    },
                    None => {
                        None
                    }
                }
            }
        }
    }
    
    pub fn run(&self, feature_path: &Path) {
        use std::sync::Arc;

        let last_panic: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let feature_path = fs::read_dir(feature_path).expect("feature path to exist");

        let mut scenarios = 0;
        let mut step_count = 0;

        for entry in feature_path {
            let mut file = File::open(entry.unwrap().path()).expect("file to open");
            let mut buffer = String::new();
            file.read_to_string(&mut buffer).unwrap();
            let feature = Feature::from(&*buffer);
            
            println!("Feature: {}\n", feature.name);

            for scenario in feature.scenarios {
                scenarios += 1;

                println!("  Scenario: {}", scenario.name);

                let mut world = Mutex::new(T::default());
                
                let mut steps = vec![];
                if let Some(ref bg) = &feature.background {
                    steps.append(&mut bg.steps.clone());
                }
                
                steps.append(&mut scenario.steps.clone());

                for step in steps.into_iter() {
                    step_count += 1;

                    let value = step.value.to_string();
                    
                    let test_type = match self.test_type(&step, &value) {
                        Some(v) => v,
                        None => {
                            println!("    {}\n      # No test found", &step.to_string());
                            continue;
                        }
                    };
                    
                    let last_panic_hook = last_panic.clone();
                    panic::set_hook(Box::new(move |info| {
                        let mut state = last_panic_hook.lock().expect("last_panic unpoisoned");
                        *state = info.location().map(|x| format!("{}:{}:{}", x.file(), x.line(), x.column()));
                    }));

                    let result = panic::catch_unwind(|| {
                        match world.lock() {
                            Ok(mut world) => {
                                match test_type {
                                    TestCaseType::Normal(t) => (t.test)(&mut *world, &step),
                                    TestCaseType::Regex(t, c) => (t.test)(&mut *world, &c, &step)
                                }
                            },
                            Err(e) => {
                                return Err(e);
                            }
                        };

                        return Ok(())
                    });

                    let _ = panic::take_hook();

                    println!("    {:<40}", &step.to_string());
                    if let Some(ref docstring) = &step.docstring {
                        println!("      \"\"\"\n      {}\n      \"\"\"", docstring);
                    }

                    match result {
                        Ok(inner) => {
                            match inner {
                                Ok(_) => {},
                                Err(_) => println!("      # Skipped due to previous error")
                            }
                        }
                        Err(any) => {
                            let mut state = last_panic.lock().expect("unpoisoned");

                            {
                                let loc = match &*state {
                                    Some(v) => &v,
                                    None => "unknown"
                                };

                                let s = if let Some(s) = any.downcast_ref::<String>() {
                                    Some(s.as_str())
                                } else if let Some(s) = any.downcast_ref::<&str>() {
                                    Some(*s)
                                } else {
                                    None
                                };
                                
                                if let Some(s) = s {
                                    if s == "not yet implemented" {
                                        println!("      # Not yet implemented");
                                    } else {
                                        println!("      # Step failed:");
                                        println!("      # {}  [{}]", &s, loc);
                                    }
                                } else {
                                    println!("      # Step failed:");
                                    println!("      # Unknown reason [{}]", loc)
                                }
                            }

                            *state = None;
                        }
                    };
                }

                println!("");
            }
        }

        println!("# Scenarios: {}", scenarios);
        println!("# Steps: {}", step_count);
    }
}

#[macro_export]
macro_rules! cucumber {
    (
        features: $featurepath:tt;
        world: $worldtype:path;
        steps: $vec:expr
    ) => {
        #[allow(unused_imports)]
        fn main() {
            use std::path::Path;
            use std::process;
            use $crate::{Steps, World};

            let path = match Path::new($featurepath).canonicalize() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{}", e);
                    eprintln!("There was an error parsing \"{}\"; aborting.", $featurepath);
                    process::exit(1);
                }
            };

            if !&path.exists() {
                eprintln!("Path {:?} does not exist; aborting.", &path);
                process::exit(1);
            }

            let tests = {
                let step_groups: Vec<Steps<$worldtype>> = $vec.iter().map(|f| f()).collect();
                let mut combined_steps = Steps::new();

                for step_group in step_groups.into_iter() {
                    combined_steps.given.extend(step_group.given);
                    combined_steps.when.extend(step_group.when);
                    combined_steps.then.extend(step_group.then);

                    combined_steps.regex.given.extend(step_group.regex.given);
                    combined_steps.regex.when.extend(step_group.regex.when);
                    combined_steps.regex.then.extend(step_group.regex.then);
                }

                combined_steps
            };
            
            tests.run(&path);
        }
    }
}

#[macro_export]
macro_rules! steps {
    (
        @gather_steps, $tests:tt,
        $ty:ident regex $name:tt $body:expr;
    ) => {
        $tests.regex.$ty.insert(
            HashableRegex(Regex::new($name).expect(&format!("{} is a valid regex", $name))),
                RegexTestCase::new($body));
    };

    (
        @gather_steps, $tests:tt,
        $ty:ident regex $name:tt $body:expr; $( $items:tt )*
    ) => {
        $tests.regex.$ty.insert(
            HashableRegex(Regex::new($name).expect(&format!("{} is a valid regex", $name))),
                RegexTestCase::new($body));

        steps!(@gather_steps, $tests, $( $items )*);
    };

    (
        @gather_steps, $tests:tt,
        $ty:ident $name:tt $body:expr;
    ) => {
        $tests.$ty.insert($name, TestCase::new($body));
    };

    (
        @gather_steps, $tests:tt,
        $ty:ident $name:tt $body:expr; $( $items:tt )*
    ) => {
        $tests.$ty.insert($name, TestCase::new($body));

        steps!(@gather_steps, $tests, $( $items )*);
    };

    (
        $( $items:tt )*
    ) => {
        #[allow(unused_imports)]
        pub fn steps<T: Default>() -> $crate::Steps<T> {
            use std::path::Path;
            use std::process;
            use $crate::regex::Regex;
            use $crate::{Steps, TestCase, RegexTestCase, HashableRegex};

            let mut tests: Steps<T> = Steps::new();
            steps!(@gather_steps, tests, $( $items )*);
            tests
        }
    };
}


#[cfg(test)]
mod tests {
    use std::default::Default;

    pub struct World {
        pub thing: bool
    }

    impl ::World for World {}

    impl Default for World {
        fn default() -> World {
            World {
                thing: false
            }
        }
    }
}

#[cfg(test)]
mod tests2 {
    use std::default::Default;

    pub struct World {
        pub thing2: bool
    }

    impl ::World for World {}

    impl Default for World {
        fn default() -> World {
            World {
                thing2: true
            }
        }
    }

    steps! {
        when "nothing" |world| {
            assert!(true);
        };
        when regex "^nothing$" |world, matches| {
            assert!(true)
        };
    }
}

#[cfg(test)]
mod tests1 {
    steps! {
        when regex "^test (.*) regex$" |world, matches| {
            println!("{}", matches[1]);
        };

        given "a thing" |world| {
            assert!(true);
        };

        when "another thing" |world| {
            assert!(false);
        };

        when "something goes right" |world| { 
            assert!(true);
        };

        then "another thing" |world| {
            assert!(true)
        };
    }
}

#[cfg(test)]
cucumber! {
    features: "./features";
    world: tests::World;
    steps: &[
        tests1::steps,
        tests2::steps
    ]
}