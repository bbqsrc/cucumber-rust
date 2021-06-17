// Copyright (c) 2018-2021  Brendan Molloy <brendan@bbqsrc.net>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::any::Any;
use std::panic;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, TryLockError};

use async_stream::stream;
use futures::{Future, FutureExt, Stream, StreamExt, TryFutureExt};
use regex::Regex;

use crate::{
    collection::StepsCollection,
    criteria::Criteria,
    cucumber::{Context, LifecycleContext, StepContext},
};
use crate::{cucumber::LifecycleFn, event::*};
use crate::{TestError, World, TEST_SKIPPED};

use super::ExampleValues;
use std::time::{Duration, Instant};

pub(crate) type TestFuture<W> = Pin<Box<dyn Future<Output = Result<W, TestError>>>>;

impl<W> From<fn(W, StepContext) -> W> for TestFunction<W> {
    fn from(f: fn(W, StepContext) -> W) -> Self {
        TestFunction::BasicSync(f)
    }
}

impl<W> From<fn(W, StepContext) -> TestFuture<W>> for TestFunction<W> {
    fn from(f: fn(W, StepContext) -> TestFuture<W>) -> Self {
        TestFunction::BasicAsync(f)
    }
}

impl<W> From<fn(W, StepContext) -> W> for StepFn<W> {
    fn from(f: fn(W, StepContext) -> W) -> Self {
        StepFn::Sync(f)
    }
}

impl<W> From<fn(W, StepContext) -> TestFuture<W>> for StepFn<W> {
    fn from(f: fn(W, StepContext) -> TestFuture<W>) -> Self {
        StepFn::Async(f)
    }
}

#[derive(Clone, Copy)]
pub enum StepFn<W> {
    Sync(fn(W, StepContext) -> W),
    Async(fn(W, StepContext) -> TestFuture<W>),
}

impl<W> From<&StepFn<W>> for TestFunction<W> {
    fn from(step: &StepFn<W>) -> Self {
        match step {
            StepFn::Sync(x) => TestFunction::BasicSync(*x),
            StepFn::Async(x) => TestFunction::BasicAsync(*x),
        }
    }
}

pub enum TestFunction<W> {
    BasicSync(fn(W, StepContext) -> W),
    BasicAsync(fn(W, StepContext) -> TestFuture<W>),
    RegexSync(fn(W, StepContext) -> W, Vec<String>),
    RegexAsync(fn(W, StepContext) -> TestFuture<W>, Vec<String>),
}

fn coerce_error(err: &(dyn Any + Send + 'static)) -> String {
    if let Some(string) = err.downcast_ref::<String>() {
        string.to_string()
    } else if let Some(string) = err.downcast_ref::<&str>() {
        (*string).to_string()
    } else {
        "(Could not resolve panic payload)".into()
    }
}

/// Stats for various event results
#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// total events seen
    pub total: u32,
    /// events skipped
    pub skipped: u32,
    /// events that passed
    pub passed: u32,
    /// events that failed
    pub failed: u32,
    /// events that timed out
    pub timed_out: u32,
}

impl Stats {
    /// Indicates this has failing states (aka failed or timed_out)
    pub fn failed(&self) -> bool {
        self.failed > 0 || self.timed_out > 0
    }
}

/// The result of the Cucumber run
#[derive(Debug, Clone)]
pub struct RunResult {
    /// the time when the run was started
    pub started: std::time::Instant,
    /// the time the run took
    pub elapsed: std::time::Duration,
    /// Stats of features of this run
    pub features: Stats,
    /// Stats of rules of this run
    pub rules: Stats,
    /// Stats of scenarios of this run
    pub scenarios: Stats,
    /// Stats of scenarios of this run
    pub steps: Stats,
    /// the scenarios that failed
    pub failed_scenarios: Vec<Rc<gherkin::Scenario>>,
    /// the scenarios that timed out
    pub timed_out_scenarios: Vec<Rc<gherkin::Scenario>>,
}

impl RunResult {
    /// Indicates this has failing states (aka failed or timed_out)
    pub fn failed(&self) -> bool {
        self.features.failed() || self.scenarios.failed()
    }
}

#[derive(Debug, Clone)]
struct StatsCollector {
    started: std::time::Instant,
    features: Stats,
    rules: Stats,
    scenarios: Stats,
    steps: Stats,
    failed_scenarios: Vec<Rc<gherkin::Scenario>>,
    timed_out_scenarios: Vec<Rc<gherkin::Scenario>>,
}

impl StatsCollector {
    fn new() -> Self {
        StatsCollector {
            started: std::time::Instant::now(),
            features: Default::default(),
            rules: Default::default(),
            scenarios: Default::default(),
            steps: Default::default(),
            failed_scenarios: Default::default(),
            timed_out_scenarios: Default::default(),
        }
    }

    fn handle_rule_event(&mut self, event: &RuleEvent) {
        match event {
            RuleEvent::Starting => {
                self.rules.total += 1;
            }
            RuleEvent::Scenario(scenario, ref event) => self.handle_scenario_event(scenario.clone(), event),
            RuleEvent::Skipped => {
                self.rules.skipped += 1;
            }
            RuleEvent::Passed => {
                self.rules.passed += 1;
            }
            RuleEvent::Failed(FailureKind::Panic) => {
                self.rules.failed += 1;
            }
            RuleEvent::Failed(FailureKind::TimedOut) => {
                self.rules.timed_out += 1;
            }
        }
    }

    fn handle_scenario_event(&mut self, scenario: Rc<gherkin::Scenario>, event: &ScenarioEvent) {
        match event {
            ScenarioEvent::Starting(_) => {
                self.scenarios.total += 1;
            }
            ScenarioEvent::Background(_, ref event) => self.handle_step_event(event),
            ScenarioEvent::Step(_, ref event) => self.handle_step_event(event),
            ScenarioEvent::Skipped => {
                self.scenarios.skipped += 1;
            }
            ScenarioEvent::Passed => {
                self.scenarios.passed += 1;
            }
            ScenarioEvent::Failed(FailureKind::Panic) => {
                self.scenarios.failed += 1;
                self.failed_scenarios.push(scenario.clone())
            }
            ScenarioEvent::Failed(FailureKind::TimedOut) => {
                self.scenarios.timed_out += 1;
            }
        }
    }

    fn handle_step_event(&mut self, event: &StepEvent) {
        self.steps.total += 1;
        match event {
            StepEvent::Starting => {
                // we don't have to count this
            }
            StepEvent::Unimplemented => {
                self.steps.skipped += 1;
            }
            StepEvent::Skipped => {
                self.steps.skipped += 1;
            }
            StepEvent::Passed(_) => {
                self.steps.passed += 1;
            }
            StepEvent::Failed(StepFailureKind::Panic(_, _)) => {
                self.steps.failed += 1;
            }
            StepEvent::Failed(StepFailureKind::TimedOut) => {
                self.steps.timed_out += 1;
            }
        }
    }

    fn handle_feature_event(&mut self, event: &FeatureEvent) {
        match event {
            FeatureEvent::Starting => {
                self.features.total += 1;
            }
            FeatureEvent::Scenario(scenario, ref event) => self.handle_scenario_event(scenario.clone(), event),
            FeatureEvent::Rule(_, ref event) => self.handle_rule_event(event),
            _ => {}
        }
    }

    fn collect(self) -> RunResult {
        let StatsCollector {
            started,
            features,
            rules,
            scenarios,
            steps,
            failed_scenarios,
            timed_out_scenarios
        } = self;

        RunResult {
            elapsed: started.elapsed(),
            started,
            features,
            rules,
            scenarios,
            steps,
            failed_scenarios,
            timed_out_scenarios
        }
    }
}

pub(crate) struct Runner<W: World> {
    context: Rc<Context>,
    functions: StepsCollection<W>,
    features: Rc<Vec<gherkin::Feature>>,
    step_timeout: Option<Duration>,
    enable_capture: bool,
    scenario_filter: Option<Regex>,
    before: Vec<(Criteria, LifecycleFn)>,
    after: Vec<(Criteria, LifecycleFn)>,
}

impl<W: World> Runner<W> {
    #[inline]
    pub fn new(
        context: Rc<Context>,
        functions: StepsCollection<W>,
        features: Rc<Vec<gherkin::Feature>>,
        step_timeout: Option<Duration>,
        enable_capture: bool,
        scenario_filter: Option<Regex>,
        before: Vec<(Criteria, LifecycleFn)>,
        after: Vec<(Criteria, LifecycleFn)>,
    ) -> Rc<Runner<W>> {
        Rc::new(Runner {
            context,
            functions,
            features,
            step_timeout,
            enable_capture,
            scenario_filter,
            before,
            after,
        })
    }

    async fn run_step(self: Rc<Self>, step: Rc<gherkin::Step>, world: W) -> TestEvent<W> {
        use std::io::prelude::*;

        let func = match self.functions.resolve(&step) {
            Some(v) => v,
            None => return TestEvent::Unimplemented,
        };

        let mut maybe_capture_handles = if self.enable_capture {
            Some((shh::stdout().unwrap(), shh::stderr().unwrap()))
        } else {
            None
        };

        // This ugly mess here catches the panics from async calls.
        let panic_info = Arc::new(std::sync::Mutex::new(None));
        let panic_info0 = Arc::clone(&panic_info);
        let step_timeout0 = self.step_timeout;
        panic::set_hook(Box::new(move |pi| {
            let panic_info = Some(PanicInfo {
                location: pi
                    .location()
                    .map(|l| Location {
                        file: l.file().to_string(),
                        line: l.line(),
                        column: l.column(),
                    })
                    .unwrap_or_else(Location::unknown),
                payload: coerce_error(pi.payload()),
            });
            if let Some(step_timeout) = step_timeout0 {
                let start_point = Instant::now();
                loop {
                    match panic_info0.try_lock() {
                        Ok(mut guard) => {
                            *guard = panic_info;
                            return;
                        }
                        Err(TryLockError::WouldBlock) => {
                            if start_point.elapsed() < step_timeout {
                                continue;
                            } else {
                                return;
                            }
                        }
                        Err(TryLockError::Poisoned(_)) => {
                            return;
                        }
                    }
                }
            } else {
                *panic_info0.lock().unwrap() = panic_info;
            }
        }));

        let context = Rc::clone(&self.context);

        let step_future = match func {
            TestFunction::BasicAsync(f) => (f)(world, StepContext::new(context, step, vec![])),
            TestFunction::RegexAsync(f, r) => (f)(world, StepContext::new(context, step, r)),

            TestFunction::BasicSync(test_fn) => std::panic::AssertUnwindSafe(async move {
                (test_fn)(world, StepContext::new(context, step, vec![]))
            })
            .catch_unwind()
            .map_err(TestError::PanicError)
            .boxed_local(),

            TestFunction::RegexSync(test_fn, matches) => std::panic::AssertUnwindSafe(async move {
                (test_fn)(world, StepContext::new(context, step, matches))
            })
            .catch_unwind()
            .map_err(TestError::PanicError)
            .boxed_local(),
        };

        let result = if let Some(step_timeout) = self.step_timeout {
            let timeout = Box::pin(async {
                futures_timer::Delay::new(step_timeout).await;
                Err(TestError::TimedOut)
            });
            futures::future::select(timeout, step_future)
                .await
                .factor_first()
                .0
        } else {
            step_future.await
        };

        let mut out = String::new();
        let mut err = String::new();
        // Note the use of `take` to move the handles into this branch so that they are
        // appropriately dropped following
        if let Some((mut stdout, mut stderr)) = maybe_capture_handles.take() {
            stdout.read_to_string(&mut out).unwrap_or_else(|_| {
                out = "Error retrieving stdout".to_string();
                0
            });
            stderr.read_to_string(&mut err).unwrap_or_else(|_| {
                err = "Error retrieving stderr".to_string();
                0
            });
        }

        let output = CapturedOutput { out, err };
        match result {
            Ok(w) => TestEvent::Success(w, output),
            Err(TestError::TimedOut) => TestEvent::Failure(StepFailureKind::TimedOut),
            Err(TestError::PanicError(e)) => {
                let e = coerce_error(&e);
                if &*e == TEST_SKIPPED {
                    return TestEvent::Skipped;
                }

                let pi = if let Some(step_timeout) = self.step_timeout {
                    let start_point = Instant::now();
                    loop {
                        match panic_info.try_lock() {
                            Ok(mut guard) => {
                                break guard.take().unwrap_or_else(PanicInfo::unknown);
                            }
                            Err(TryLockError::WouldBlock) => {
                                if start_point.elapsed() < step_timeout {
                                    futures_timer::Delay::new(Duration::from_micros(10)).await;
                                    continue;
                                } else {
                                    break PanicInfo::unknown();
                                }
                            }
                            Err(TryLockError::Poisoned(_)) => break PanicInfo::unknown(),
                        }
                    }
                } else {
                    let mut guard = panic_info.lock().unwrap();
                    guard.take().unwrap_or_else(PanicInfo::unknown)
                };
                TestEvent::Failure(StepFailureKind::Panic(output, pi))
            }
        }
    }

    fn run_feature(self: Rc<Self>, feature: Rc<gherkin::Feature>) -> FeatureStream {
        Box::pin(stream! {
            yield FeatureEvent::Starting;

            let context = LifecycleContext {
                context: self.context.clone(),
                feature: Rc::clone(&feature),
                rule: None,
                scenario: None,
            };

            for (criteria, handler) in self.before.iter() {
                if !criteria.context().is_feature() {
                    continue;
                }

                if criteria.eval(&*feature, None, None) {
                    (handler)(context.clone()).await;
                }
            }

            for scenario in feature.scenarios.iter() {
                // If regex filter fails, skip the scenario
                if let Some(ref regex) = self.scenario_filter {
                    if !regex.is_match(&scenario.name) {
                        continue;
                    }
                }

                let examples = ExampleValues::from_examples(&scenario.examples);
                for example_values in examples {
                    let this = Rc::clone(&self);
                    let scenario = Rc::new(scenario.clone());

                    let mut stream = this.run_scenario(Rc::clone(&scenario), None, Rc::clone(&feature), example_values);

                    while let Some(event) = stream.next().await {
                        yield FeatureEvent::Scenario(Rc::clone(&scenario), event);
                    }
                }
            }

            for rule in feature.rules.iter() {
                let this = Rc::clone(&self);
                let rule = Rc::new(rule.clone());

                let mut stream = this.run_rule(Rc::clone(&rule), Rc::clone(&feature));

                while let Some(event) = stream.next().await {
                    yield FeatureEvent::Rule(Rc::clone(&rule), event);
                }
            }

            for (criteria, handler) in self.after.iter() {
                if !criteria.context().is_feature() {
                    continue;
                }

                if criteria.eval(&*feature, None, None) {
                    (handler)(context.clone()).await;
                }
            }

            yield FeatureEvent::Finished;
        })
    }

    fn run_rule(
        self: Rc<Self>,
        rule: Rc<gherkin::Rule>,
        feature: Rc<gherkin::Feature>,
    ) -> RuleStream {
        Box::pin(stream! {
            yield RuleEvent::Starting;

            let context = LifecycleContext {
                context: self.context.clone(),
                feature: Rc::clone(&feature),
                rule: Some(Rc::clone(&rule)),
                scenario: None,
            };

            for (criteria, handler) in self.before.iter() {
                if !criteria.context().is_rule() {
                    continue;
                }

                if criteria.eval(&*feature, Some(&*rule), None) {
                    (handler)(context.clone()).await;
                }
            }

            let mut return_event = None;

            for scenario in rule.scenarios.iter() {
                let this = Rc::clone(&self);
                let scenario = Rc::new(scenario.clone());

                let mut stream = this.run_scenario(Rc::clone(&scenario), Some(Rc::clone(&rule)), Rc::clone(&feature), ExampleValues::empty());

                while let Some(event) = stream.next().await {
                    match event {
                        ScenarioEvent::Failed(FailureKind::Panic) => { return_event = Some(RuleEvent::Failed(FailureKind::Panic)); },
                        ScenarioEvent::Failed(FailureKind::TimedOut) => { return_event = Some(RuleEvent::Failed(FailureKind::TimedOut)); },
                        ScenarioEvent::Passed if return_event.is_none() => { return_event = Some(RuleEvent::Passed); },
                        ScenarioEvent::Skipped if return_event == Some(RuleEvent::Passed) => { return_event = Some(RuleEvent::Skipped); }
                        _ => {}
                    }
                    yield RuleEvent::Scenario(Rc::clone(&scenario), event);
                }
            }

            for (criteria, handler) in self.after.iter() {
                if !criteria.context().is_rule() {
                    continue;
                }

                if criteria.eval(&*feature, Some(&*rule), None) {
                    (handler)(context.clone()).await;
                }
            }

            yield return_event.unwrap_or(RuleEvent::Skipped);
        })
    }

    fn run_scenario(
        self: Rc<Self>,
        scenario: Rc<gherkin::Scenario>,
        rule: Option<Rc<gherkin::Rule>>,
        feature: Rc<gherkin::Feature>,
        example: super::ExampleValues,
    ) -> ScenarioStream {
        Box::pin(stream! {
            yield ScenarioEvent::Starting(example.clone());

            let context = LifecycleContext {
                context: self.context.clone(),
                feature: Rc::clone(&feature),
                rule: rule.clone(),
                scenario: Some(Rc::clone(&scenario)),
            };

            for (criteria, handler) in self.before.iter() {
                if !criteria.context().is_scenario() {
                    continue;
                }

                if criteria.eval(&*feature, rule.as_ref().map(|x| &**x), Some(&*scenario)) {
                    (handler)(context.clone()).await;
                }
            }

            let mut world = Some(W::new().await.unwrap());

            let mut is_success = true;

            if let Some(steps) = feature.background.as_ref().map(|x| &x.steps) {
                for step in steps.iter() {
                    let this = Rc::clone(&self);
                    let step = Rc::new(step.clone());

                    yield ScenarioEvent::Background(Rc::clone(&step), StepEvent::Starting);

                    let result = this.run_step(Rc::clone(&step), world.take().unwrap()).await;

                    match result {
                        TestEvent::Success(w, output) => {
                            yield ScenarioEvent::Background(Rc::clone(&step), StepEvent::Passed(output));
                            // Pass world result for current step to next step.
                            world = Some(w);
                        }
                        TestEvent::Failure(StepFailureKind::Panic(output, e)) => {
                            yield ScenarioEvent::Background(Rc::clone(&step), StepEvent::Failed(StepFailureKind::Panic(output, e)));
                            yield ScenarioEvent::Failed(FailureKind::Panic);
                            is_success = false;
                            break;
                        },
                        TestEvent::Failure(StepFailureKind::TimedOut) => {
                            yield ScenarioEvent::Background(Rc::clone(&step), StepEvent::Failed(StepFailureKind::TimedOut));
                            yield ScenarioEvent::Failed(FailureKind::TimedOut);
                            is_success = false;
                            break;
                        }
                        TestEvent::Skipped => {
                            yield ScenarioEvent::Background(Rc::clone(&step), StepEvent::Skipped);
                            yield ScenarioEvent::Skipped;
                            is_success = false;
                            break;
                        }
                        TestEvent::Unimplemented => {
                            yield ScenarioEvent::Background(Rc::clone(&step), StepEvent::Unimplemented);
                            yield ScenarioEvent::Skipped;
                            is_success = false;
                            break;
                        }
                    }
                }
            }

            if is_success {
                for step in scenario.steps.iter() {
                    let this = Rc::clone(&self);

                    let mut step = step.clone();
                    if !example.is_empty() {
                        step.value = example.insert_values(&step.value);
                    }
                    let step = Rc::new(step);

                    yield ScenarioEvent::Step(Rc::clone(&step), StepEvent::Starting);

                    let result = this.run_step(Rc::clone(&step), world.take().unwrap()).await;

                    match result {
                        TestEvent::Success(w, output) => {
                            yield ScenarioEvent::Step(Rc::clone(&step), StepEvent::Passed(output));
                            // Pass world result for current step to next step.
                            world = Some(w);
                        }
                        TestEvent::Failure(StepFailureKind::Panic(output, e)) => {
                            yield ScenarioEvent::Step(Rc::clone(&step), StepEvent::Failed(StepFailureKind::Panic(output, e)));
                            yield ScenarioEvent::Failed(FailureKind::Panic);
                            is_success = false;
                            break;
                        },
                        TestEvent::Failure(StepFailureKind::TimedOut) => {
                            yield ScenarioEvent::Step(Rc::clone(&step), StepEvent::Failed(StepFailureKind::TimedOut));
                            yield ScenarioEvent::Failed(FailureKind::TimedOut);
                            is_success = false;
                            break;
                        }
                        TestEvent::Skipped => {
                            yield ScenarioEvent::Step(Rc::clone(&step), StepEvent::Skipped);
                            yield ScenarioEvent::Skipped;
                            is_success = false;
                            break;
                        }
                        TestEvent::Unimplemented => {
                            yield ScenarioEvent::Step(Rc::clone(&step), StepEvent::Unimplemented);
                            yield ScenarioEvent::Skipped;
                            is_success = false;
                            break;
                        }
                    }
                }
            }

            for (criteria, handler) in self.after.iter() {
                if !criteria.context().is_scenario() {
                    continue;
                }

                if criteria.eval(&*feature, rule.as_ref().map(|x| &**x), Some(&*scenario)) {
                    (handler)(context.clone()).await;
                }
            }

            if is_success {
                yield ScenarioEvent::Passed;
            }
        })
    }

    pub fn run(self: Rc<Self>) -> CucumberStream {
        Box::pin(stream! {
            let mut stats = StatsCollector::new();
            yield CucumberEvent::Starting;

            let features = self.features.iter().cloned().map(Rc::new).collect::<Vec<_>>();
            for feature in features.into_iter() {
                let this = Rc::clone(&self);
                let mut stream = this.run_feature(Rc::clone(&feature));

                while let Some(event) = stream.next().await {
                    stats.handle_feature_event(&event);
                    yield CucumberEvent::Feature(Rc::clone(&feature), event);
                }
            }

            yield CucumberEvent::Finished(stats.collect());
        })
    }
}

type CucumberStream = Pin<Box<dyn Stream<Item = CucumberEvent>>>;
type FeatureStream = Pin<Box<dyn Stream<Item = FeatureEvent>>>;
type RuleStream = Pin<Box<dyn Stream<Item = RuleEvent>>>;
type ScenarioStream = Pin<Box<dyn Stream<Item = ScenarioEvent>>>;
