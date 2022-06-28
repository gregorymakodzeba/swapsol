//! Program state processor

use crate::constraints::{SWAP_CONSTRAINTS};

use crate::{
    curve::{
        base::{SwapCurve, CurveType},
        constant_product::ConstantProductCurve,
        calculator::{RoundDirection, TradeDirection, INITIAL_SWAP_POOL_AMOUNT},
        fees::Fees,
    },
    error::AmmError,
    amm_instruction::{
        DepositInstruction, DepositSingleTokenTypeExactAmountIn, InitializeInstruction, SwapInstruction,
        AmmInstruction, WithdrawInstruction, WithdrawSingleTokenTypeExactAmountOut, UpdateStateInstruction
    },
    amm_stats::{AmmStatus, ProgramState, SwapV1, SwapVersion},
};
use std::str::FromStr;
use num_traits::FromPrimitive;
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    decode_error::DecodeError,
    entrypoint::ProgramResult,
    msg,
    program::invoke_signed,
    program::invoke,
    program_error::{PrintProgramError, ProgramError},
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    system_instruction,
    sysvar::{rent::Rent, Sysvar},

};
use std::convert::TryInto;

/// address to change the program state at first
pub const INITIAL_STATE_OWNER: &str = "DjXkZxNWUoGsL87rbWRFVPmoxN1FKXUWpinUyN921PwQ";

/// Seed for prgoram state
pub const AMM_STATE_SEED:&str = "AmmState";

/// WSOL MINT Address
pub const WSOL_MINT_ADDRESS:&str = "So11111111111111111111111111111111111111112";
/// Neonomad LP Token Decimal
pub const LP_MINT_DECIMALS:u8 = 8;
/// 0.001 in actual amount
pub const MIN_LP_SUPPLY:u128 = 100000;
/// Program state handler.
pub struct Processor {}
impl Processor {
    /// Unpacks a spl_token `Account`.
    pub fn unpack_token_account(
        account_info: &AccountInfo,
        token_program_id: &Pubkey,
    ) -> Result<spl_token::state::Account, AmmError> {
        if account_info.owner != token_program_id {
            Err(AmmError::IncorrectTokenProgramId)
        } else {
            spl_token::state::Account::unpack(&account_info.data.borrow())
                .map_err(|_| AmmError::ExpectedAccount)
        }
    }

    /// create ab account for global state
    pub fn create_or_allocate_account_raw<'a>(
        program_id: Pubkey,
        new_account_info: &AccountInfo<'a>,
        rent_sysvar_info: &AccountInfo<'a>,
        system_program_info: &AccountInfo<'a>,
        payer_info: &AccountInfo<'a>,
        size: usize,
        signer_seeds: &[&[u8]],
    ) -> Result<(), ProgramError> {
        let rent = &Rent::from_account_info(rent_sysvar_info)?;
        let required_lamports = rent
            .minimum_balance(size)
            .max(1)
            .saturating_sub(new_account_info.lamports());
    
        if required_lamports > 0 {
            msg!("Transfer {} lamports to the new account", required_lamports);
            invoke(
                &system_instruction::transfer(&payer_info.key, new_account_info.key, required_lamports),
                &[
                    payer_info.clone(),
                    new_account_info.clone(),
                    system_program_info.clone(),
                ],
            )?;
        }
    
        msg!("Allocate space for the account");
        invoke_signed(
            &system_instruction::allocate(new_account_info.key, size.try_into().unwrap()),
            &[new_account_info.clone(), system_program_info.clone()],
            &[&signer_seeds],
        )?;
    
        msg!("Assign the account to the owning program");
        invoke_signed(
            &system_instruction::assign(new_account_info.key, &program_id),
            &[new_account_info.clone(), system_program_info.clone()],
            &[&signer_seeds],
        )?;
        msg!("Completed assignation!");
    
        Ok(())
    }
    /// Unpacks a spl_token `Mint`.
    pub fn unpack_mint(
        account_info: &AccountInfo,
        token_program_id: &Pubkey,
    ) -> Result<spl_token::state::Mint, AmmError> {
        if account_info.owner != token_program_id {
            Err(AmmError::IncorrectTokenProgramId)
        } else {
            spl_token::state::Mint::unpack(&account_info.data.borrow())
                .map_err(|_| AmmError::ExpectedMint)
        }
    }

    /// check if the program account address is valid
    pub fn check_state_account(program_id:&Pubkey, key: &Pubkey)->Result<(), ProgramError>{
        let seeds = [
            AMM_STATE_SEED.as_bytes(),
            program_id.as_ref(),
        ];

        let (program_data_key, _bump) = Pubkey::find_program_address(&seeds, program_id);
        if program_data_key != *key {
            return Err(AmmError::InvalidStateAddress.into());
        }
        else {
            Ok(())
        }
    }

    /// Calculates the authority id by generating a program address.
    pub fn authority_id(
        program_id: &Pubkey,
        my_info: &Pubkey,
        nonce: u8,
    ) -> Result<Pubkey, AmmError> {
        Pubkey::create_program_address(&[&my_info.to_bytes()[..32], &[nonce]], program_id)
            .or(Err(AmmError::InvalidProgramAddress))
    }

    /// Issue a spl_token `Burn` instruction.
    pub fn token_burn<'a>(
        swap: &Pubkey,
        token_program: AccountInfo<'a>,
        burn_account: AccountInfo<'a>,
        mint: AccountInfo<'a>,
        authority: AccountInfo<'a>,
        nonce: u8,
        amount: u64,
    ) -> Result<(), ProgramError> {
        let swap_bytes = swap.to_bytes();
        let authority_signature_seeds = [&swap_bytes[..32], &[nonce]];
        let signers = &[&authority_signature_seeds[..]];

        let ix = spl_token::instruction::burn(
            token_program.key,
            burn_account.key,
            mint.key,
            authority.key,
            &[],
            amount,
        )?;

        invoke_signed(
            &ix,
            &[burn_account, mint, authority, token_program],
            signers,
        )
    }

    /// Issue a spl_token `MintTo` instruction.
    pub fn token_mint_to<'a>(
        swap: &Pubkey,
        token_program: AccountInfo<'a>,
        mint: AccountInfo<'a>,
        destination: AccountInfo<'a>,
        authority: AccountInfo<'a>,
        nonce: u8,
        amount: u64,
    ) -> Result<(), ProgramError> {
        let swap_bytes = swap.to_bytes();
        let authority_signature_seeds = [&swap_bytes[..32], &[nonce]];
        let signers = &[&authority_signature_seeds[..]];
        let ix = spl_token::instruction::mint_to(
            token_program.key,
            mint.key,
            destination.key,
            authority.key,
            &[],
            amount,
        )?;

        invoke_signed(&ix, &[mint, destination, authority, token_program], signers)
    }

    /// Issue a spl_token `Transfer` instruction.
    pub fn token_transfer<'a>(
        swap: &Pubkey,
        token_program: AccountInfo<'a>,
        source: AccountInfo<'a>,
        destination: AccountInfo<'a>,
        authority: AccountInfo<'a>,
        nonce: u8,
        amount: u64,
    ) -> Result<(), ProgramError> {
        let swap_bytes = swap.to_bytes();
        let authority_signature_seeds = [&swap_bytes[..32], &[nonce]];
        let signers = &[&authority_signature_seeds[..]];
        let ix = spl_token::instruction::transfer(
            token_program.key,
            source.key,
            destination.key,
            authority.key,
            &[],
            amount,
        )?;
        invoke_signed(
            &ix,
            &[source, destination, authority, token_program],
            signers,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn check_accounts(
        token_swap: &dyn AmmStatus,
        program_id: &Pubkey,
        swap_account_info: &AccountInfo,
        authority_info: &AccountInfo,
        token_a_info: &AccountInfo,
        token_b_info: &AccountInfo,
        pool_mint_info: &AccountInfo,
        token_program_info: &AccountInfo,
        user_token_a_info: Option<&AccountInfo>,
        user_token_b_info: Option<&AccountInfo>,
    ) -> ProgramResult {
        if swap_account_info.owner != program_id {
            return Err(ProgramError::IncorrectProgramId);
        }
        if *authority_info.key
            != Self::authority_id(program_id, swap_account_info.key, token_swap.nonce())?
        {
            return Err(AmmError::InvalidProgramAddress.into());
        }
        if *token_a_info.key != *token_swap.token_a_account() {
            return Err(AmmError::IncorrectSwapAccount.into());
        }
        if *token_b_info.key != *token_swap.token_b_account() {
            return Err(AmmError::IncorrectSwapAccount.into());
        }
        if *pool_mint_info.key != *token_swap.pool_mint() {
            return Err(AmmError::IncorrectPoolMint.into());
        }
        if *token_program_info.key != *token_swap.token_program_id() {
            return Err(AmmError::IncorrectTokenProgramId.into());
        }
        if let Some(user_token_a_info) = user_token_a_info {
            if token_a_info.key == user_token_a_info.key {
                return Err(AmmError::InvalidInput.into());
            }
        }
        if let Some(user_token_b_info) = user_token_b_info {
            if token_b_info.key == user_token_b_info.key {
                return Err(AmmError::InvalidInput.into());
            }
        }
        Ok(())
    }

    /// Processes an [Initialize](enum.Instruction.html).
    pub fn process_update_state(
        program_id: &Pubkey,
        initial_supply:u64,
        fees: Fees,
        swap_curve: SwapCurve,
        accounts: &[AccountInfo]
    ) -> ProgramResult {

        //load account info
        let account_info_iter = &mut accounts.iter();
        let state_info = next_account_info(account_info_iter)?;

        let cur_state_owner_info = next_account_info(account_info_iter)?;
        let new_state_owner_info = next_account_info(account_info_iter)?;

        let fee_owner_info = next_account_info(account_info_iter)?;

        let system_info = next_account_info(account_info_iter)?;
        let rent_info = next_account_info(account_info_iter)?;

        Self::check_state_account(program_id, state_info.key)?;
        
        if !cur_state_owner_info.is_signer{
            return Err(AmmError::InvalidSigner.into());
        }

        let seeds = [
            AMM_STATE_SEED.as_bytes(),
            program_id.as_ref(),
        ];

        let (_pda_key, bump) = Pubkey::find_program_address(&seeds, program_id);
        
        if state_info.data_is_empty(){
            let size = ProgramState::get_packed_len();

            Self::create_or_allocate_account_raw(
                *program_id,
                state_info,
                rent_info,
                system_info,
                cur_state_owner_info,
                size,
                &[
                    AMM_STATE_SEED.as_bytes(),
                    program_id.as_ref(),
                    &[bump],
                ],
            )?;
        }

        let mut program_state = ProgramState::unpack_from_slice(&state_info.data.borrow())?;

        if program_state.is_initialized == false
        {
            program_state.state_owner = Pubkey::from_str(INITIAL_STATE_OWNER).unwrap();
            program_state.is_initialized = true;
            program_state.fees = Fees {
                fixed_fee_numerator: SWAP_CONSTRAINTS.fees.fixed_fee_numerator,
                return_fee_numerator: SWAP_CONSTRAINTS.fees.return_fee_numerator,
                fee_denominator: SWAP_CONSTRAINTS.fees.fee_denominator,
            };
            program_state.fee_owner = Pubkey::from_str(SWAP_CONSTRAINTS.owner_key).unwrap();
            program_state.initial_supply = INITIAL_SWAP_POOL_AMOUNT;
            program_state.swap_curve = SwapCurve {
                    curve_type: CurveType::ConstantProduct,
                    calculator: Box::new(
                        ConstantProductCurve{}
                    )
                };
            program_state.pack_into_slice(&mut &mut state_info.data.borrow_mut()[..]);
        }
        
        if program_state.state_owner != *cur_state_owner_info.key
        {
            return Err(AmmError::InvalidStateOwner.into());
        }

        SWAP_CONSTRAINTS.validate_curve(&swap_curve)?;
        SWAP_CONSTRAINTS.validate_fees(&fees)?;

        fees.validate()?;
        swap_curve.calculator.validate()?;

        //Save the program state
        let obj = ProgramState{
            is_initialized:true,
            initial_supply: initial_supply,
            state_owner: *new_state_owner_info.key,
            fee_owner: *fee_owner_info.key,
            fees,
            swap_curve,
        };
        obj.pack_into_slice(&mut &mut state_info.data.borrow_mut()[..]);
        Ok(())
    }

    /// Processes an [Initialize](enum.Instruction.html).
    pub fn process_initialize(
        program_id: &Pubkey,
        nonce: u8,
        accounts: &[AccountInfo]
    ) -> ProgramResult {
        
        //load account info
        let account_info_iter = &mut accounts.iter();
        let swap_info = next_account_info(account_info_iter)?;
        let authority_info = next_account_info(account_info_iter)?;
        let state_info = next_account_info(account_info_iter)?;
        let amm_id_info = next_account_info(account_info_iter)?;
        let token_a_info = next_account_info(account_info_iter)?;
        let token_b_info = next_account_info(account_info_iter)?;
        let pool_mint_info = next_account_info(account_info_iter)?;
        let destination_info = next_account_info(account_info_iter)?;

        let market_info = next_account_info(account_info_iter)?;        

        let token_program_info = next_account_info(account_info_iter)?;
        let dex_program_info = next_account_info(account_info_iter)?;
        let cur_state_owner_info = next_account_info(account_info_iter)?;

        //validate account info
        let token_program_id = *token_program_info.key;
        if SwapVersion::is_initialized(&swap_info.data.borrow()) {
            return Err(AmmError::AlreadyInUse.into());
        }

        if *authority_info.key != Self::authority_id(program_id, swap_info.key, nonce)? {
            return Err(AmmError::InvalidProgramAddress.into());
        }

        Self::check_state_account(program_id, state_info.key)?;
        
        let state = ProgramState::unpack_from_slice(&state_info.data.borrow())?;
        if state.is_initialized() == false
        {
            return Err(AmmError::NotInitializedState.into());
        }
        if !cur_state_owner_info.is_signer{
            return Err(AmmError::InvalidSigner.into());
        }
        if *cur_state_owner_info.key != state.state_owner {
            return Err(AmmError::InvalidOwner.into());
        }

        let token_a = Self::unpack_token_account(token_a_info, &token_program_id)?;
        let token_b = Self::unpack_token_account(token_b_info, &token_program_id)?;

        let destination = Self::unpack_token_account(destination_info, &token_program_id)?;
        let pool_mint = Self::unpack_mint(pool_mint_info, &token_program_id)?;
        if *authority_info.key != token_a.owner {
            return Err(AmmError::InvalidOwner.into());
        }
        if *authority_info.key != token_b.owner {
            return Err(AmmError::InvalidOwner.into());
        }
        if *authority_info.key == destination.owner {
            return Err(AmmError::InvalidOutputOwner.into());
        }
        
        if COption::Some(*authority_info.key) != pool_mint.mint_authority {
            return Err(AmmError::InvalidOwner.into());
        }

        if token_a.mint == token_b.mint {
            return Err(AmmError::RepeatedMint.into());
        }

        let swap_curve = state.swap_curve();

        swap_curve.calculator.validate_supply(token_a.amount, token_b.amount)?;

        if token_a.delegate.is_some() {
            return Err(AmmError::InvalidDelegate.into());
        }
        if token_b.delegate.is_some() {
            return Err(AmmError::InvalidDelegate.into());
        }
        if token_a.is_frozen(){
            return Err(AmmError::InvalidFreezeAuthority.into());
        }
        if token_b.is_frozen(){
            return Err(AmmError::InvalidFreezeAuthority.into());
        }
        if token_a.close_authority.is_some() {
            return Err(AmmError::InvalidCloseAuthority.into());
        }
        if token_b.close_authority.is_some() {
            return Err(AmmError::InvalidCloseAuthority.into());
        }
        if pool_mint.decimals != LP_MINT_DECIMALS{
            return Err(AmmError::InvalidDecimals.into());
        }
        if pool_mint.supply != 0 {
            return Err(AmmError::InvalidSupply.into());
        }
        if pool_mint.freeze_authority.is_some() {
            return Err(AmmError::InvalidFreezeAuthority.into());
        }
        if market_info.owner != &(*dex_program_info.key){
            return Err(AmmError::IncorrectMarketOwnerAccount.into());
        }

        let initial_amount = state.initial_supply();

        //Mint Initial supply
        Self::token_mint_to(
            swap_info.key,
            token_program_info.clone(),
            pool_mint_info.clone(),
            destination_info.clone(),
            authority_info.clone(),
            nonce,
            initial_amount,
        )?;

        //Save the pool account info
        let obj = SwapVersion::SwapV1(SwapV1 {
            is_initialized: true,
            nonce,
            amm_id: *amm_id_info.key,
            dex_program_id: *dex_program_info.key,
            market_id: *market_info.key,
            token_program_id,
            token_a: *token_a_info.key,
            token_b: *token_b_info.key,
            pool_mint: *pool_mint_info.key,
            token_a_mint: token_a.mint,
            token_b_mint: token_b.mint
        });
        SwapVersion::pack(obj, &mut swap_info.data.borrow_mut())?;
        Ok(())
    }

    /// Processes an [Swap](enum.Instruction.html).
    pub fn process_swap(
        program_id: &Pubkey,
        amount_in: u64,
        minimum_amount_out: u64,
        accounts: &[AccountInfo],
    ) -> ProgramResult {
        
        //load account info
        let account_info_iter = &mut accounts.iter();
        let swap_info = next_account_info(account_info_iter)?;
        let authority_info = next_account_info(account_info_iter)?;
        let user_transfer_authority_info = next_account_info(account_info_iter)?;
        let state_info = next_account_info(account_info_iter)?;
        let source_info = next_account_info(account_info_iter)?;
        let swap_source_info = next_account_info(account_info_iter)?;
        let swap_destination_info = next_account_info(account_info_iter)?;
        let destination_info = next_account_info(account_info_iter)?;
        let pool_mint_info = next_account_info(account_info_iter)?;
        let fixed_fee_account_info = next_account_info(account_info_iter)?;
        let fixed_fee_wallet_info = next_account_info(account_info_iter)?;
        let token_program_info = next_account_info(account_info_iter)?;
        let system_program_info = next_account_info(account_info_iter)?;

        //validate account info
        if swap_info.owner != program_id {
            return Err(ProgramError::IncorrectProgramId);
        }
        let token_swap = SwapVersion::unpack(&swap_info.data.borrow())?;

        if *authority_info.key != Self::authority_id(program_id, swap_info.key, token_swap.nonce())?
        {
            return Err(AmmError::InvalidProgramAddress.into());
        }

        Self::check_state_account(program_id, state_info.key)?;
        
        let state = ProgramState::unpack_from_slice(&state_info.data.borrow())?;
        if state.is_initialized() == false
        {
            return Err(AmmError::NotInitializedState.into());
        }

        if !(*swap_source_info.key == *token_swap.token_a_account()
            || *swap_source_info.key == *token_swap.token_b_account())
        {
            return Err(AmmError::IncorrectSwapAccount.into());
        }
        if !(*swap_destination_info.key == *token_swap.token_a_account()
            || *swap_destination_info.key == *token_swap.token_b_account())
        {
            return Err(AmmError::IncorrectSwapAccount.into());
        }
        if *swap_source_info.key == *swap_destination_info.key {
            return Err(AmmError::InvalidInput.into());
        }
        if swap_source_info.key == source_info.key {
            return Err(AmmError::InvalidInput.into());
        }
        if swap_destination_info.key == destination_info.key {
            return Err(AmmError::InvalidInput.into());
        }
        if *pool_mint_info.key != *token_swap.pool_mint() {
            return Err(AmmError::IncorrectPoolMint.into());
        }
        if *token_program_info.key != *token_swap.token_program_id() {
            return Err(AmmError::IncorrectTokenProgramId.into());
        }
        let trade_direction = if *swap_source_info.key == *token_swap.token_a_account() {
            TradeDirection::AtoB
        } else {
            TradeDirection::BtoA
        };

        let source_account = Self::unpack_token_account(swap_source_info, token_swap.token_program_id())?;
        let dest_account = Self::unpack_token_account(swap_destination_info, token_swap.token_program_id())?;
        
        // let pool_mint = Self::unpack_mint(pool_mint_info, token_swap.token_program_id())?;

        let wsol_mint =  Pubkey::from_str(WSOL_MINT_ADDRESS).unwrap();

        if *state.fee_owner() != *fixed_fee_wallet_info.key
        {
            return Err(AmmError::InvalidOwner.into());
        }

        //check the fee accounts are set corretly
        if wsol_mint != source_account.mint
        {
            let fee_account = Self::unpack_token_account(fixed_fee_account_info, token_swap.token_program_id())?;
            if *state.fee_owner() != fee_account.owner || source_account.mint != fee_account.mint
            {
                return Err(AmmError::IncorrectFeeAccount.into());
            }
        }

        let result = state.swap_curve()
            .swap(
                to_u128(amount_in)?,
                to_u128(source_account.amount)?,
                to_u128(dest_account.amount)?,
                trade_direction,
                state.fees(),
            )
            .ok_or(AmmError::ZeroTradingTokens)?;

        if result.destination_amount_swapped < to_u128(minimum_amount_out)? {
            return Err(AmmError::ExceededSlippage.into());
        }
        //@zhaohui
        // let (swap_token_a_amount, swap_token_b_amount) = match trade_direction {
        //     TradeDirection::AtoB => (
        //         result.new_swap_source_amount,
        //         result.new_swap_destination_amount,
        //     ),
        //     TradeDirection::BtoA => (
        //         result.new_swap_destination_amount,
        //         result.new_swap_source_amount,
        //     ),
        // };

        Self::token_transfer(
            swap_info.key,
            token_program_info.clone(),
            source_info.clone(),
            swap_source_info.clone(),
            user_transfer_authority_info.clone(),
            token_swap.nonce(),
            to_u64(result.source_amount_swapped-result.owner_fee)?,
        )?;

        //if the fee token is WSOL, then transfer SOL to fee account directly
        if source_account.mint == wsol_mint
        {
            let source = user_transfer_authority_info.clone();
            let destination = fixed_fee_wallet_info.clone();
            invoke(
                &system_instruction::transfer(
                    source.key,
                    destination.key,
                    to_u64(result.owner_fee)?,
                ),
                &[source, destination, system_program_info.clone()]
            )?;
        }
        else
        {
            //otherwise transfer SPL_Token
            Self::token_transfer(
                swap_info.key,
                token_program_info.clone(),
                source_info.clone(),
                fixed_fee_account_info.clone(),
                user_transfer_authority_info.clone(),
                token_swap.nonce(),
                to_u64(result.owner_fee)?,
            )?;
        }
        
        //Transfer pc token from pool
        Self::token_transfer(
            swap_info.key,
            token_program_info.clone(),
            swap_destination_info.clone(),
            destination_info.clone(),
            authority_info.clone(),
            token_swap.nonce(),
            to_u64(result.destination_amount_swapped)?,
        )?;

        Ok(())
    }

    /// Processes an [DepositAllTokenTypes](enum.Instruction.html).
    pub fn process_deposit_all_token_types(
        program_id: &Pubkey,
        pool_token_amount: u64,
        maximum_token_a_amount: u64,
        maximum_token_b_amount: u64,
        accounts: &[AccountInfo],
    ) -> ProgramResult {
        
        //load account info
        let account_info_iter = &mut accounts.iter();
        let swap_info = next_account_info(account_info_iter)?;
        let authority_info = next_account_info(account_info_iter)?;
        let user_transfer_authority_info = next_account_info(account_info_iter)?;
        let state_info = next_account_info(account_info_iter)?;
        let source_a_info = next_account_info(account_info_iter)?;
        let source_b_info = next_account_info(account_info_iter)?;
        let token_a_info = next_account_info(account_info_iter)?;
        let token_b_info = next_account_info(account_info_iter)?;
        let pool_mint_info = next_account_info(account_info_iter)?;
        let dest_info = next_account_info(account_info_iter)?;
        let token_program_info = next_account_info(account_info_iter)?;

        //validate account
        let token_swap = SwapVersion::unpack(&swap_info.data.borrow())?;

        Self::check_state_account(program_id, state_info.key)?;
        
        let state = ProgramState::unpack_from_slice(&state_info.data.borrow())?;
        if state.is_initialized() == false
        {
            return Err(AmmError::NotInitializedState.into());
        }

        let calculator = &state.swap_curve().calculator;
        if !calculator.allows_deposits() {
            return Err(AmmError::UnsupportedCurveOperation.into());
        }
        Self::check_accounts(
            token_swap.as_ref(),
            program_id,
            swap_info,
            authority_info,
            token_a_info,
            token_b_info,
            pool_mint_info,
            token_program_info,
            Some(source_a_info),
            Some(source_b_info)
        )?;

        let token_a = Self::unpack_token_account(token_a_info, token_swap.token_program_id())?;
        let token_b = Self::unpack_token_account(token_b_info, token_swap.token_program_id())?;
        let pool_mint = Self::unpack_mint(pool_mint_info, token_swap.token_program_id())?;
        let current_pool_mint_supply = to_u128(pool_mint.supply)?;
        let (pool_token_amount, pool_mint_supply) = if current_pool_mint_supply > 0 {
            (to_u128(pool_token_amount)?, current_pool_mint_supply)
        } else {
            (to_u128(state.initial_supply())?, to_u128(state.initial_supply())?)
        };

        let results = calculator
            .pool_tokens_to_trading_tokens(
                pool_token_amount,
                pool_mint_supply,
                to_u128(token_a.amount)?,
                to_u128(token_b.amount)?,
                RoundDirection::Ceiling,
            )
            .ok_or(AmmError::ZeroTradingTokens)?;
        let token_a_amount = to_u64(results.token_a_amount)?;
        if token_a_amount > maximum_token_a_amount {
            return Err(AmmError::ExceededSlippage.into());
        }
        if token_a_amount == 0 {
            return Err(AmmError::ZeroTradingTokens.into());
        }
        let token_b_amount = to_u64(results.token_b_amount)?;
        if token_b_amount > maximum_token_b_amount {
            return Err(AmmError::ExceededSlippage.into());
        }
        if token_b_amount == 0 {
            return Err(AmmError::ZeroTradingTokens.into());
        }

        let pool_token_amount = to_u64(pool_token_amount)?;
        //transfer token to pool
        Self::token_transfer(
            swap_info.key,
            token_program_info.clone(),
            source_a_info.clone(),
            token_a_info.clone(),
            user_transfer_authority_info.clone(),
            token_swap.nonce(),
            token_a_amount,
        )?;
        Self::token_transfer(
            swap_info.key,
            token_program_info.clone(),
            source_b_info.clone(),
            token_b_info.clone(),
            user_transfer_authority_info.clone(),
            token_swap.nonce(),
            token_b_amount,
        )?;
        //mint lp token to wallet
        Self::token_mint_to(
            swap_info.key,
            token_program_info.clone(),
            pool_mint_info.clone(),
            dest_info.clone(),
            authority_info.clone(),
            token_swap.nonce(),
            pool_token_amount,
        )?;

        Ok(())
    }

    /// Processes an [WithdrawAllTokenTypes](enum.Instruction.html).
    pub fn process_withdraw_all_token_types(
        program_id: &Pubkey,
        pool_token_amount: u64,
        minimum_token_a_amount: u64,
        minimum_token_b_amount: u64,
        accounts: &[AccountInfo],
    ) -> ProgramResult {
        //load account info
        let account_info_iter = &mut accounts.iter();
        let swap_info = next_account_info(account_info_iter)?;
        let authority_info = next_account_info(account_info_iter)?;
        let user_transfer_authority_info = next_account_info(account_info_iter)?;
        let state_info = next_account_info(account_info_iter)?;
        let pool_mint_info = next_account_info(account_info_iter)?;
        let source_info = next_account_info(account_info_iter)?;
        let token_a_info = next_account_info(account_info_iter)?;
        let token_b_info = next_account_info(account_info_iter)?;
        let dest_token_a_info = next_account_info(account_info_iter)?;
        let dest_token_b_info = next_account_info(account_info_iter)?;
        let token_program_info = next_account_info(account_info_iter)?;

        //validate accounts
        let token_swap = SwapVersion::unpack(&swap_info.data.borrow())?;

        Self::check_state_account(program_id, state_info.key)?;
        
        let state = ProgramState::unpack_from_slice(&state_info.data.borrow())?;
        if state.is_initialized() == false
        {
            return Err(AmmError::NotInitializedState.into());
        }

        Self::check_accounts(
            token_swap.as_ref(),
            program_id,
            swap_info,
            authority_info,
            token_a_info,
            token_b_info,
            pool_mint_info,
            token_program_info,
            Some(dest_token_a_info),
            Some(dest_token_b_info),
        )?;

        let token_a = Self::unpack_token_account(token_a_info, token_swap.token_program_id())?;
        let token_b = Self::unpack_token_account(token_b_info, token_swap.token_program_id())?;
        let pool_mint = Self::unpack_mint(pool_mint_info, token_swap.token_program_id())?;

        let calculator = &state.swap_curve().calculator;

        let withdraw_fee: u128 = 0;
        // if *fixed_fee_account_info.key == *source_info.key {
        //     // withdrawing from the fee account, don't assess withdraw fee
        //     0
        // } else {
        //     token_swap
        //         .fees()
        //         .owner_withdraw_fee(to_u128(pool_token_amount)?)
        //         .ok_or(AmmError::FeeCalculationFailure)?
        // };
        
        let mut pool_token_amount = to_u128(pool_token_amount)?
            .checked_sub(withdraw_fee)
            .ok_or(AmmError::CalculationFailure)?;
        
        //Check the minimum lp token amount to burn
        let max_pool_token_amount = to_u128(pool_mint.supply)?.checked_sub(MIN_LP_SUPPLY).ok_or(AmmError::CalculationFailure)?;
        pool_token_amount = std::cmp::min(pool_token_amount, max_pool_token_amount);
        
        let results = calculator
            .pool_tokens_to_trading_tokens(
                pool_token_amount,
                to_u128(pool_mint.supply)?,
                to_u128(token_a.amount)?,
                to_u128(token_b.amount)?,
                RoundDirection::Floor,
            )
            .ok_or(AmmError::ZeroTradingTokens)?;
        let token_a_amount = to_u64(results.token_a_amount)?;
        let token_a_amount = std::cmp::min(token_a.amount, token_a_amount);
        if token_a_amount < minimum_token_a_amount {
            return Err(AmmError::ExceededSlippage.into());
        }
        if token_a_amount == 0 && token_a.amount != 0 {
            return Err(AmmError::ZeroTradingTokens.into());
        }
        let token_b_amount = to_u64(results.token_b_amount)?;
        let token_b_amount = std::cmp::min(token_b.amount, token_b_amount);
        if token_b_amount < minimum_token_b_amount {
            return Err(AmmError::ExceededSlippage.into());
        }
        if token_b_amount == 0 && token_b.amount != 0 {
            return Err(AmmError::ZeroTradingTokens.into());
        }

        // if withdraw_fee > 0 {
        //     Self::token_transfer(
        //         swap_info.key,
        //         token_program_info.clone(),
        //         source_info.clone(),
        //         fixed_fee_account_info.clone(),
        //         user_transfer_authority_info.clone(),
        //         token_swap.nonce(),
        //         to_u64(withdraw_fee)?,
        //     )?;
        // }
        //remove lp token from wallet
        Self::token_burn(
            swap_info.key,
            token_program_info.clone(),
            source_info.clone(),
            pool_mint_info.clone(),
            user_transfer_authority_info.clone(),
            token_swap.nonce(),
            to_u64(pool_token_amount)?,
        )?;
        //transfer coin token to wallet
        if token_a_amount > 0 {
            Self::token_transfer(
                swap_info.key,
                token_program_info.clone(),
                token_a_info.clone(),
                dest_token_a_info.clone(),
                authority_info.clone(),
                token_swap.nonce(),
                token_a_amount,
            )?;
        }
        //transfer pc token to wallet
        if token_b_amount > 0 {
            Self::token_transfer(
                swap_info.key,
                token_program_info.clone(),
                token_b_info.clone(),
                dest_token_b_info.clone(),
                authority_info.clone(),
                token_swap.nonce(),
                token_b_amount,
            )?;
        }
        Ok(())
    }

    /// Processes DepositSingleTokenTypeExactAmountIn
    pub fn process_deposit_single_token_type_exact_amount_in(
        program_id: &Pubkey,
        source_token_amount: u64,
        minimum_pool_token_amount: u64,
        accounts: &[AccountInfo],
    ) -> ProgramResult {
        let account_info_iter = &mut accounts.iter();
        let swap_info = next_account_info(account_info_iter)?;
        let authority_info = next_account_info(account_info_iter)?;
        let user_transfer_authority_info = next_account_info(account_info_iter)?;
        let state_info = next_account_info(account_info_iter)?;
        let source_info = next_account_info(account_info_iter)?;
        let swap_token_a_info = next_account_info(account_info_iter)?;
        let swap_token_b_info = next_account_info(account_info_iter)?;
        let pool_mint_info = next_account_info(account_info_iter)?;
        let destination_info = next_account_info(account_info_iter)?;
        let token_program_info = next_account_info(account_info_iter)?;

        let token_swap = SwapVersion::unpack(&swap_info.data.borrow())?;

        Self::check_state_account(program_id, state_info.key)?;
        
        let state = ProgramState::unpack_from_slice(&state_info.data.borrow())?;
        if state.is_initialized() == false
        {
            return Err(AmmError::NotInitializedState.into());
        }

        let source_account =
            Self::unpack_token_account(source_info, token_swap.token_program_id())?;
        let swap_token_a =
            Self::unpack_token_account(swap_token_a_info, token_swap.token_program_id())?;
        let swap_token_b =
            Self::unpack_token_account(swap_token_b_info, token_swap.token_program_id())?;

        let trade_direction = if source_account.mint == swap_token_a.mint {
            TradeDirection::AtoB
        } else if source_account.mint == swap_token_b.mint {
            TradeDirection::BtoA
        } else {
            return Err(AmmError::IncorrectSwapAccount.into());
        };

        let (source_a_info, source_b_info) = match trade_direction {
            TradeDirection::AtoB => (Some(source_info), None),
            TradeDirection::BtoA => (None, Some(source_info)),
        };

        Self::check_accounts(
            token_swap.as_ref(),
            program_id,
            swap_info,
            authority_info,
            swap_token_a_info,
            swap_token_b_info,
            pool_mint_info,
            token_program_info,
            source_a_info,
            source_b_info,
        )?;

        let pool_mint = Self::unpack_mint(pool_mint_info, token_swap.token_program_id())?;
        let pool_mint_supply = to_u128(pool_mint.supply)?;
        let pool_token_amount = if pool_mint_supply > 0 {
            state
                .swap_curve()
                .deposit_single_token_type(
                    to_u128(source_token_amount)?,
                    to_u128(swap_token_a.amount)?,
                    to_u128(swap_token_b.amount)?,
                    pool_mint_supply,
                    trade_direction,
                    state.fees(),
                )
                .ok_or(AmmError::ZeroTradingTokens)?
        } else {
            to_u128(state.initial_supply())?
        };

        let pool_token_amount = to_u64(pool_token_amount)?;
        if pool_token_amount < minimum_pool_token_amount {
            return Err(AmmError::ExceededSlippage.into());
        }
        if pool_token_amount == 0 {
            return Err(AmmError::ZeroTradingTokens.into());
        }

        match trade_direction {
            TradeDirection::AtoB => {
                Self::token_transfer(
                    swap_info.key,
                    token_program_info.clone(),
                    source_info.clone(),
                    swap_token_a_info.clone(),
                    user_transfer_authority_info.clone(),
                    token_swap.nonce(),
                    source_token_amount,
                )?;
            }
            TradeDirection::BtoA => {
                Self::token_transfer(
                    swap_info.key,
                    token_program_info.clone(),
                    source_info.clone(),
                    swap_token_b_info.clone(),
                    user_transfer_authority_info.clone(),
                    token_swap.nonce(),
                    source_token_amount,
                )?;
            }
        }
        Self::token_mint_to(
            swap_info.key,
            token_program_info.clone(),
            pool_mint_info.clone(),
            destination_info.clone(),
            authority_info.clone(),
            token_swap.nonce(),
            pool_token_amount,
        )?;

        Ok(())
    }

    /// Processes a [WithdrawSingleTokenTypeExactAmountOut](enum.Instruction.html).
    pub fn process_withdraw_single_token_type_exact_amount_out(
        program_id: &Pubkey,
        destination_token_amount: u64,
        maximum_pool_token_amount: u64,
        accounts: &[AccountInfo],
    ) -> ProgramResult {
        let account_info_iter = &mut accounts.iter();
        let swap_info = next_account_info(account_info_iter)?;
        let authority_info = next_account_info(account_info_iter)?;
        let user_transfer_authority_info = next_account_info(account_info_iter)?;
        let state_info = next_account_info(account_info_iter)?;
        let pool_mint_info = next_account_info(account_info_iter)?;
        let source_info = next_account_info(account_info_iter)?;
        let swap_token_a_info = next_account_info(account_info_iter)?;
        let swap_token_b_info = next_account_info(account_info_iter)?;
        let destination_info = next_account_info(account_info_iter)?;
        let token_program_info = next_account_info(account_info_iter)?;

        let token_swap = SwapVersion::unpack(&swap_info.data.borrow())?;

        Self::check_state_account(program_id, state_info.key)?;
        
        let state = ProgramState::unpack_from_slice(&state_info.data.borrow())?;
        if state.is_initialized() == false
        {
            return Err(AmmError::NotInitializedState.into());
        }

        let destination_account =
            Self::unpack_token_account(destination_info, token_swap.token_program_id())?;
        let swap_token_a =
            Self::unpack_token_account(swap_token_a_info, token_swap.token_program_id())?;
        let swap_token_b =
            Self::unpack_token_account(swap_token_b_info, token_swap.token_program_id())?;

        let trade_direction = if destination_account.mint == swap_token_a.mint {
            TradeDirection::AtoB
        } else if destination_account.mint == swap_token_b.mint {
            TradeDirection::BtoA
        } else {
            return Err(AmmError::IncorrectSwapAccount.into());
        };

        let (destination_a_info, destination_b_info) = match trade_direction {
            TradeDirection::AtoB => (Some(destination_info), None),
            TradeDirection::BtoA => (None, Some(destination_info)),
        };
        Self::check_accounts(
            token_swap.as_ref(),
            program_id,
            swap_info,
            authority_info,
            swap_token_a_info,
            swap_token_b_info,
            pool_mint_info,
            token_program_info,
            destination_a_info,
            destination_b_info,
        )?;

        let pool_mint = Self::unpack_mint(pool_mint_info, token_swap.token_program_id())?;
        let pool_mint_supply = to_u128(pool_mint.supply)?;
        let swap_token_a_amount = to_u128(swap_token_a.amount)?;
        let swap_token_b_amount = to_u128(swap_token_b.amount)?;

        let burn_pool_token_amount = state
            .swap_curve()
            .withdraw_single_token_type_exact_out(
                to_u128(destination_token_amount)?,
                swap_token_a_amount,
                swap_token_b_amount,
                pool_mint_supply,
                trade_direction,
                state.fees(),
            )
            .ok_or(AmmError::ZeroTradingTokens)?;

        let withdraw_fee: u128 = 0;
        // if *fixed_fee_account_info.key == *source_info.key {
        //     // withdrawing from the fee account, don't assess withdraw fee
        //     0
        // } else {
        //     token_swap
        //         .fees()
        //         .owner_withdraw_fee(burn_pool_token_amount)
        //         .ok_or(AmmError::FeeCalculationFailure)?
        // };
        let pool_token_amount = burn_pool_token_amount
            .checked_add(withdraw_fee)
            .ok_or(AmmError::CalculationFailure)?;

        if to_u64(pool_token_amount)? > maximum_pool_token_amount {
            return Err(AmmError::ExceededSlippage.into());
        }
        if pool_token_amount == 0 {
            return Err(AmmError::ZeroTradingTokens.into());
        }

        // if withdraw_fee > 0 {
        //     Self::token_transfer(
        //         swap_info.key,
        //         token_program_info.clone(),
        //         source_info.clone(),
        //         fixed_fee_account_info.clone(),
        //         user_transfer_authority_info.clone(),
        //         token_swap.nonce(),
        //         to_u64(withdraw_fee)?,
        //     )?;
        // }
        Self::token_burn(
            swap_info.key,
            token_program_info.clone(),
            source_info.clone(),
            pool_mint_info.clone(),
            user_transfer_authority_info.clone(),
            token_swap.nonce(),
            to_u64(burn_pool_token_amount)?,
        )?;

        match trade_direction {
            TradeDirection::AtoB => {
                Self::token_transfer(
                    swap_info.key,
                    token_program_info.clone(),
                    swap_token_a_info.clone(),
                    destination_info.clone(),
                    authority_info.clone(),
                    token_swap.nonce(),
                    destination_token_amount,
                )?;
            }
            TradeDirection::BtoA => {
                Self::token_transfer(
                    swap_info.key,
                    token_program_info.clone(),
                    swap_token_b_info.clone(),
                    destination_info.clone(),
                    authority_info.clone(),
                    token_swap.nonce(),
                    destination_token_amount,
                )?;
            }
        }

        Ok(())
    }

    /// Processes an [Instruction](enum.Instruction.html).
    pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], input: &[u8]) -> ProgramResult {
        let instruction = AmmInstruction::unpack(input)?;
        match instruction {
            AmmInstruction::UpdateState(UpdateStateInstruction {
                initial_supply,
                fees,
                swap_curve,
            }) => {
                msg!("Instruction: UpdateState");
                Self::process_update_state(
                    program_id,
                    initial_supply,
                    fees,
                    swap_curve,
                    accounts,
                )
            }
            AmmInstruction::Initialize(InitializeInstruction {
                nonce,
            }) => {
                msg!("Instruction: Init");
                Self::process_initialize(
                    program_id,
                    nonce,
                    accounts,
                )
            }
            AmmInstruction::Swap(SwapInstruction {
                amount_in,
                minimum_amount_out,
            }) => {
                msg!("Instruction: Swap");
                Self::process_swap(program_id, amount_in, minimum_amount_out, accounts)
            }
            AmmInstruction::DepositAllTokenTypes(DepositInstruction {
                pool_token_amount,
                maximum_token_a_amount,
                maximum_token_b_amount,
            }) => {
                msg!("Instruction: DepositAllTokenTypes");
                Self::process_deposit_all_token_types(
                    program_id,
                    pool_token_amount,
                    maximum_token_a_amount,
                    maximum_token_b_amount,
                    accounts,
                )
            }
            AmmInstruction::WithdrawAllTokenTypes(WithdrawInstruction {
                pool_token_amount,
                minimum_token_a_amount,
                minimum_token_b_amount,
            }) => {
                msg!("Instruction: WithdrawAllTokenTypes");
                Self::process_withdraw_all_token_types(
                    program_id,
                    pool_token_amount,
                    minimum_token_a_amount,
                    minimum_token_b_amount,
                    accounts,
                )
            }
            AmmInstruction::DepositSingleTokenTypeExactAmountIn(
                DepositSingleTokenTypeExactAmountIn {
                    source_token_amount,
                    minimum_pool_token_amount,
                },
            ) => {
                msg!("Instruction: DepositSingleTokenTypeExactAmountIn");
                Self::process_deposit_single_token_type_exact_amount_in(
                    program_id,
                    source_token_amount,
                    minimum_pool_token_amount,
                    accounts,
                )
            }
            AmmInstruction::WithdrawSingleTokenTypeExactAmountOut(
                WithdrawSingleTokenTypeExactAmountOut {
                    destination_token_amount,
                    maximum_pool_token_amount,
                },
            ) => {
                msg!("Instruction: WithdrawSingleTokenTypeExactAmountOut");
                Self::process_withdraw_single_token_type_exact_amount_out(
                    program_id,
                    destination_token_amount,
                    maximum_pool_token_amount,
                    accounts,
                )
            }
        }
    }
}

impl PrintProgramError for AmmError {
    fn print<E>(&self)
    where
        E: 'static + std::error::Error + DecodeError<E> + PrintProgramError + FromPrimitive,
    {
        match self {
            AmmError::AlreadyInUse => msg!("Error: Swap account already in use"),
            AmmError::InvalidProgramAddress => {
                msg!("Error: Invalid program address generated from nonce and key")
            }            
            AmmError::InvalidStateAddress => {
                msg!("Error: Invalid state address generated from seed")
            }
            AmmError::InvalidStateOwner => {
                msg!("Error: The input account is not a owner of state account")
            }
            AmmError::InvalidOwner => {
                msg!("Error: The input account owner is not the program address")
            }
            AmmError::InvalidOutputOwner => {
                msg!("Error: Output pool account owner cannot be the program address")
            }
            AmmError::ExpectedMint => msg!("Error: Deserialized account is not an SPL Token mint"),
            AmmError::ExpectedAccount => {
                msg!("Error: Deserialized account is not an SPL Token account")
            }
            AmmError::EmptySupply => msg!("Error: Input token account empty"),
            AmmError::InvalidDecimals => msg!("Error: Pool token mint doesn't have exact decimal"),
            AmmError::InvalidSupply => msg!("Error: Pool token mint has a non-zero supply"),
            AmmError::RepeatedMint => msg!("Error: Swap input token accounts have the same mint"),
            AmmError::InvalidDelegate => msg!("Error: Token account has a delegate"),
            AmmError::InvalidInput => msg!("Error: InvalidInput"),
            AmmError::IncorrectSwapAccount => {
                msg!("Error: Address of the provided swap token account is incorrect")
            }
            AmmError::IncorrectPoolMint => {
                msg!("Error: Address of the provided pool token mint is incorrect")
            }
            AmmError::InvalidOutput => msg!("Error: InvalidOutput"),
            AmmError::CalculationFailure => msg!("Error: CalculationFailure"),
            AmmError::InvalidInstruction => msg!("Error: InvalidInstruction"),
            AmmError::ExceededSlippage => {
                msg!("Error: Swap instruction exceeds desired slippage limit")
            }
            AmmError::InvalidCloseAuthority => msg!("Error: Token account has a close authority"),
            AmmError::InvalidFreezeAuthority => msg!("Error: Pool token mint has a freeze authority"),
            AmmError::IncorrectMarketOwnerAccount => msg!("Error: Owner of Market account is incorrect"),
            AmmError::InvalidSigner => msg!("State owner should be the signer"),
            AmmError::NotInitializedState => msg!("Program State should be initialized before creating pool"),

            AmmError::IncorrectFeeAccount => msg!("Error: Pool fee token account incorrect"),
            AmmError::ZeroTradingTokens => {
                msg!("Error: Given pool token amount results in zero trading tokens")
            }
            AmmError::FeeCalculationFailure => msg!(
                "Error: The fee calculation failed due to overflow, underflow, or unexpected 0"
            ),
            AmmError::ConversionFailure => msg!("Error: Conversion to or from u64 failed."),
            AmmError::InvalidFee => {
                msg!("Error: The provided fee does not match the program owner's constraints")
            }
            AmmError::IncorrectTokenProgramId => {
                msg!("Error: The provided token program does not match the token program expected by the swap")
            }
            AmmError::UnsupportedCurveType => {
                msg!("Error: The provided curve type is not supported by the program owner")
            }
            AmmError::InvalidCurve => {
                msg!("Error: The provided curve parameters are invalid")
            }
            AmmError::UnsupportedCurveOperation => {
                msg!("Error: The operation cannot be performed on the given curve")
            }
        }
    }
}

fn to_u128(val: u64) -> Result<u128, AmmError> {
    val.try_into().map_err(|_| AmmError::ConversionFailure)
}

fn to_u64(val: u128) -> Result<u64, AmmError> {
    val.try_into().map_err(|_| AmmError::ConversionFailure)
}
